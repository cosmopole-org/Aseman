//! RocksDB-backed OpenRaft log store and state machine.
//!
//! The raft log and state-machine metadata live in a dedicated RocksDB at
//! `<storage_root>/cluster/raft-db` (column families `meta`, `logs`, `sm`),
//! fully separate from the node's main key-value database. Applying a
//! committed entry hands the [`ClusterCommand`] to the node-side
//! [`CommandApplier`], which writes into the main database / filesystem —
//! that is where replicated shell state, distributed VM state, and deployed
//! creature artifacts materialize on every instance.
//!
//! Snapshots capture the state machine's own metadata plus the replicated
//! cluster config store. The bulk application state is not re-shipped in
//! snapshots — it is already persisted by every instance at apply time; a
//! brand-new instance should join the cluster before log compaction trims
//! the history it needs, or be seeded from a storage backup first (see
//! `wiki/08-consensus-federation-cluster.md`).

use std::fmt::Debug;
use std::io::Cursor;
use std::ops::RangeBounds;
use std::path::Path;
use std::sync::Arc;

use openraft::storage::{LogFlushed, LogState, RaftLogStorage, RaftStateMachine, Snapshot};
use openraft::{
    BasicNode, Entry, EntryPayload, LogId, OptionalSend, RaftLogReader, RaftSnapshotBuilder,
    SnapshotMeta, StorageError, StorageIOError, StoredMembership, Vote,
};
use rocksdb::{ColumnFamily, ColumnFamilyDescriptor, Options, DB};
use serde::{Deserialize, Serialize};

use super::command::{ClusterCommand, ClusterResponse, TypeConfig};

type NodeId = u64;
type StorageResult<T> = Result<T, StorageError<NodeId>>;

/// Node-side sink for committed commands. Implemented in `mod.rs` where the
/// `ICore` handle lives.
pub trait CommandApplier: Send + Sync + 'static {
    fn apply(&self, cmd: &ClusterCommand) -> ClusterResponse;
}

fn id_to_bin(id: u64) -> [u8; 8] {
    id.to_be_bytes()
}

fn bin_to_id(buf: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&buf[..8]);
    u64::from_be_bytes(b)
}

fn read_err(e: impl std::error::Error + 'static) -> StorageError<NodeId> {
    StorageIOError::read(&e).into()
}

fn write_err(e: impl std::error::Error + 'static) -> StorageError<NodeId> {
    StorageIOError::write(&e).into()
}

/// Open (or create) the cluster RocksDB with its column families.
pub fn open_db(dir: &Path) -> anyhow::Result<Arc<DB>> {
    std::fs::create_dir_all(dir)?;
    // Bounded-memory options + shared block cache (see
    // `crate::drivers::rocks_tuning`) so the raft log/state DB can't grow
    // resident memory without bound either.
    let mut opts = crate::drivers::rocks_tuning::tuned_options();
    opts.create_missing_column_families(true);
    opts.create_if_missing(true);
    let cfs = vec![
        ColumnFamilyDescriptor::new("meta", crate::drivers::rocks_tuning::tuned_options()),
        ColumnFamilyDescriptor::new("logs", crate::drivers::rocks_tuning::tuned_options()),
        ColumnFamilyDescriptor::new("sm", crate::drivers::rocks_tuning::tuned_options()),
    ];
    Ok(Arc::new(DB::open_cf_descriptors(&opts, dir, cfs)?))
}

// ─────────────────────────── Log storage ───────────────────────────────────

#[derive(Clone)]
pub struct LogStore {
    db: Arc<DB>,
}

impl LogStore {
    pub fn new(db: Arc<DB>) -> Self {
        LogStore { db }
    }

    fn cf_meta(&self) -> &ColumnFamily {
        self.db.cf_handle("meta").expect("meta column family")
    }

    fn cf_logs(&self) -> &ColumnFamily {
        self.db.cf_handle("logs").expect("logs column family")
    }

    fn get_meta<T: for<'de> Deserialize<'de>>(&self, key: &str) -> StorageResult<Option<T>> {
        let bytes = self.db.get_cf(self.cf_meta(), key).map_err(read_err)?;
        match bytes {
            Some(b) => Ok(Some(serde_json::from_slice(&b).map_err(read_err)?)),
            None => Ok(None),
        }
    }

    fn put_meta<T: Serialize>(&self, key: &str, value: &T) -> StorageResult<()> {
        let bytes = serde_json::to_vec(value).map_err(write_err)?;
        self.db
            .put_cf(self.cf_meta(), key, bytes)
            .map_err(write_err)?;
        self.db.flush_wal(true).map_err(write_err)?;
        Ok(())
    }

    fn last_log_id(&self) -> StorageResult<Option<LogId<NodeId>>> {
        let mut it = self
            .db
            .iterator_cf(self.cf_logs(), rocksdb::IteratorMode::End);
        match it.next() {
            Some(Ok((_, v))) => {
                let entry: Entry<TypeConfig> = serde_json::from_slice(&v).map_err(read_err)?;
                Ok(Some(entry.log_id))
            }
            _ => Ok(None),
        }
    }
}

impl RaftLogReader<TypeConfig> for LogStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + OptionalSend>(
        &mut self,
        range: RB,
    ) -> StorageResult<Vec<Entry<TypeConfig>>> {
        let start = match range.start_bound() {
            std::ops::Bound::Included(x) => *x,
            std::ops::Bound::Excluded(x) => x + 1,
            std::ops::Bound::Unbounded => 0,
        };
        let mut out = Vec::new();
        let start_key = id_to_bin(start);
        let it = self.db.iterator_cf(
            self.cf_logs(),
            rocksdb::IteratorMode::From(&start_key, rocksdb::Direction::Forward),
        );
        for item in it {
            let (k, v) = item.map_err(read_err)?;
            let index = bin_to_id(&k);
            if !range.contains(&index) {
                break;
            }
            let entry: Entry<TypeConfig> = serde_json::from_slice(&v).map_err(read_err)?;
            out.push(entry);
        }
        Ok(out)
    }
}

impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> StorageResult<LogState<TypeConfig>> {
        let last_purged: Option<LogId<NodeId>> = self.get_meta("last_purged")?;
        let last = match self.last_log_id()? {
            Some(id) => Some(id),
            None => last_purged,
        };
        Ok(LogState {
            last_purged_log_id: last_purged,
            last_log_id: last,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> StorageResult<()> {
        self.put_meta("vote", vote)
    }

    async fn read_vote(&mut self) -> StorageResult<Option<Vote<NodeId>>> {
        self.get_meta("vote")
    }

    async fn save_committed(&mut self, committed: Option<LogId<NodeId>>) -> StorageResult<()> {
        self.put_meta("committed", &committed)
    }

    async fn read_committed(&mut self) -> StorageResult<Option<LogId<NodeId>>> {
        Ok(self
            .get_meta::<Option<LogId<NodeId>>>("committed")?
            .flatten())
    }

    async fn append<I>(&mut self, entries: I, callback: LogFlushed<TypeConfig>) -> StorageResult<()>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        for entry in entries {
            let bytes = serde_json::to_vec(&entry).map_err(write_err)?;
            self.db
                .put_cf(self.cf_logs(), id_to_bin(entry.log_id.index), bytes)
                .map_err(write_err)?;
        }
        let res = self.db.flush_wal(true);
        callback.log_io_completed(res.map_err(|e| std::io::Error::other(e.to_string())));
        Ok(())
    }

    async fn truncate(&mut self, log_id: LogId<NodeId>) -> StorageResult<()> {
        self.db
            .delete_range_cf(self.cf_logs(), id_to_bin(log_id.index), id_to_bin(u64::MAX))
            .map_err(write_err)?;
        self.db.flush_wal(true).map_err(write_err)?;
        Ok(())
    }

    async fn purge(&mut self, log_id: LogId<NodeId>) -> StorageResult<()> {
        self.put_meta("last_purged", &log_id)?;
        self.db
            .delete_range_cf(self.cf_logs(), id_to_bin(0), id_to_bin(log_id.index + 1))
            .map_err(write_err)?;
        Ok(())
    }
}

// ─────────────────────────── State machine ─────────────────────────────────

/// The snapshot blob: the state machine's replicated metadata. Application
/// state is persisted on every instance at apply time (see module docs).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SmSnapshot {
    last_applied: Option<LogId<NodeId>>,
    membership: StoredMembership<NodeId, BasicNode>,
    shared_config: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredSnapshotMeta {
    meta: SnapshotMeta<NodeId, BasicNode>,
}

pub struct StateMachineStore {
    db: Arc<DB>,
    applier: Arc<dyn CommandApplier>,
    state: SmSnapshot,
}

impl StateMachineStore {
    pub fn new(db: Arc<DB>, applier: Arc<dyn CommandApplier>) -> StorageResult<Self> {
        let state = match db
            .get_cf(db.cf_handle("sm").expect("sm column family"), "state")
            .map_err(read_err)?
        {
            Some(b) => serde_json::from_slice(&b).map_err(read_err)?,
            None => SmSnapshot::default(),
        };
        Ok(StateMachineStore { db, applier, state })
    }

    fn cf_sm(&self) -> &ColumnFamily {
        self.db.cf_handle("sm").expect("sm column family")
    }

    fn persist_state(&self) -> StorageResult<()> {
        let bytes = serde_json::to_vec(&self.state).map_err(write_err)?;
        self.db
            .put_cf(self.cf_sm(), "state", bytes)
            .map_err(write_err)?;
        self.db.flush_wal(true).map_err(write_err)?;
        Ok(())
    }

    /// The replicated cluster-wide config store (read side for telemetry).
    pub fn shared_config(&self) -> std::collections::BTreeMap<String, serde_json::Value> {
        self.state.shared_config.clone()
    }
}

pub struct SnapshotBuilder {
    db: Arc<DB>,
    state: SmSnapshot,
}

impl RaftSnapshotBuilder<TypeConfig> for SnapshotBuilder {
    async fn build_snapshot(&mut self) -> StorageResult<Snapshot<TypeConfig>> {
        let data = serde_json::to_vec(&self.state).map_err(write_err)?;
        let snapshot_id = format!(
            "{}-{}",
            self.state
                .last_applied
                .map(|l| l.to_string())
                .unwrap_or_else(|| "none".to_string()),
            uuid::Uuid::new_v4()
        );
        let meta = SnapshotMeta {
            last_log_id: self.state.last_applied,
            last_membership: self.state.membership.clone(),
            snapshot_id,
        };
        let cf = self.db.cf_handle("sm").expect("sm column family");
        self.db
            .put_cf(cf, "snapshot_data", &data)
            .map_err(write_err)?;
        self.db
            .put_cf(
                cf,
                "snapshot_meta",
                serde_json::to_vec(&StoredSnapshotMeta { meta: meta.clone() })
                    .map_err(write_err)?,
            )
            .map_err(write_err)?;
        self.db.flush_wal(true).map_err(write_err)?;
        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        })
    }
}

impl RaftStateMachine<TypeConfig> for StateMachineStore {
    type SnapshotBuilder = SnapshotBuilder;

    async fn applied_state(
        &mut self,
    ) -> StorageResult<(Option<LogId<NodeId>>, StoredMembership<NodeId, BasicNode>)> {
        Ok((self.state.last_applied, self.state.membership.clone()))
    }

    async fn apply<I>(&mut self, entries: I) -> StorageResult<Vec<ClusterResponse>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let mut replies = Vec::new();
        for entry in entries {
            self.state.last_applied = Some(entry.log_id);
            let reply = match entry.payload {
                EntryPayload::Blank => ClusterResponse::ok(),
                EntryPayload::Membership(m) => {
                    self.state.membership = StoredMembership::new(Some(entry.log_id), m);
                    ClusterResponse::ok()
                }
                EntryPayload::Normal(cmd) => {
                    if let ClusterCommand::ConfigPut { key, value } = &cmd {
                        self.state.shared_config.insert(key.clone(), value.clone());
                    }
                    self.applier.apply(&cmd)
                }
            };
            replies.push(reply);
        }
        self.persist_state()?;
        Ok(replies)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        SnapshotBuilder {
            db: self.db.clone(),
            state: self.state.clone(),
        }
    }

    async fn begin_receiving_snapshot(&mut self) -> StorageResult<Box<Cursor<Vec<u8>>>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<NodeId, BasicNode>,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> StorageResult<()> {
        let data = snapshot.into_inner();
        let mut incoming: SmSnapshot = serde_json::from_slice(&data).map_err(read_err)?;
        incoming.last_applied = meta.last_log_id;
        incoming.membership = meta.last_membership.clone();
        self.state = incoming;
        self.persist_state()?;
        let cf = self.cf_sm();
        self.db
            .put_cf(cf, "snapshot_data", &data)
            .map_err(write_err)?;
        self.db
            .put_cf(
                cf,
                "snapshot_meta",
                serde_json::to_vec(&StoredSnapshotMeta { meta: meta.clone() })
                    .map_err(write_err)?,
            )
            .map_err(write_err)?;
        self.db.flush_wal(true).map_err(write_err)?;
        Ok(())
    }

    async fn get_current_snapshot(&mut self) -> StorageResult<Option<Snapshot<TypeConfig>>> {
        let cf = self.cf_sm();
        let meta_bytes = self.db.get_cf(cf, "snapshot_meta").map_err(read_err)?;
        let data = self.db.get_cf(cf, "snapshot_data").map_err(read_err)?;
        match (meta_bytes, data) {
            (Some(mb), Some(db_)) => {
                let stored: StoredSnapshotMeta = serde_json::from_slice(&mb).map_err(read_err)?;
                Ok(Some(Snapshot {
                    meta: stored.meta,
                    snapshot: Box::new(Cursor::new(db_)),
                }))
            }
            _ => Ok(None),
        }
    }
}
