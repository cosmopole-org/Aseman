//! The RocksDB storage provider plugin (ADR 0036): model queries over the capsule
//! store's secondary indexes, and transactions that buffer writes, read their own
//! writes, and commit as one conditional batch (a Raft log entry in cluster mode,
//! checked in log order on every replica).

use crate::capsule_store::{RocksDbCapsuleStore, invalid, storage};
use crate::cluster;
use crate::consensus_log::RocksDbConsensusLogStorage;
use crate::model_index::{self, Plan, Scan};
use crate::{LegacyKvStore, RocksDbKvStore};
use aseman_capsule::CapsuleStoreError;
use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleId};
use aseman_ports::consensus_log::ConsensusLogStorage;
use aseman_storage::provider::{
    CapsuleTransaction, Mode, ProviderPlugin, ProviderSettings, StorageProvider,
};
use aseman_storage::schema::{Model, Schema};
use aseman_storage::{FindMany, Id, Row, StorageError, StorageResult, Where, codec, eval};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The plugin name `ASEMAN_CORE_STORAGE_PROVIDER` selects.
pub const NAME: &str = "rocksdb";
/// Rows a query without `take` returns at most.
pub const DEFAULT_TAKE: u64 = aseman_contracts::capsule::MAX_QUERY_LIMIT as u64;
/// The model store's directory under the storage root.
pub const DATA_DIRECTORY: &str = "data";
/// The consensus logs' directory under the storage root.
pub const CONSENSUS_DIRECTORY: &str = "consensus";
/// Where Hashgraph kept each chain's log before ADR 0036:
/// `{storage_root}/chains/{work_chain}/{shard}/rocksdb_db`.
pub const LEGACY_CHAINS_DIRECTORY: &str = "chains";
pub const LEGACY_LOG_DIRECTORY: &str = "rocksdb_db";

/// A consensus log from before ADR 0036: its new relative name and its directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyConsensusLog {
    pub name: String,
    pub path: PathBuf,
}

/// The consensus logs Hashgraph kept under `storage_root` before ADR 0036, each with
/// the name it takes now (`chains/{work_chain}/{shard}`).
pub fn legacy_consensus_logs(storage_root: &Path) -> std::io::Result<Vec<LegacyConsensusLog>> {
    let mut logs = Vec::new();
    let chains = storage_root.join(LEGACY_CHAINS_DIRECTORY);
    let Ok(work_chains) = std::fs::read_dir(&chains) else {
        return Ok(logs);
    };
    for work_chain in work_chains {
        let work_chain = work_chain?.path();
        let Ok(shards) = std::fs::read_dir(&work_chain) else {
            continue;
        };
        for shard in shards {
            let shard = shard?.path();
            let path = shard.join(LEGACY_LOG_DIRECTORY);
            if !path.join("CURRENT").is_file() {
                continue;
            }
            if let (Some(work_chain), Some(shard)) = (work_chain.file_name(), shard.file_name()) {
                logs.push(LegacyConsensusLog {
                    name: format!(
                        "{LEGACY_CHAINS_DIRECTORY}/{}/{}",
                        work_chain.to_string_lossy(),
                        shard.to_string_lossy()
                    ),
                    path,
                });
            }
        }
    }
    logs.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(logs)
}

/// `path` renamed aside to `{path}--retired--{timestamp}` (kept for inspection).
pub fn retire_directory(path: &Path) -> std::io::Result<PathBuf> {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(
        "--retired--{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs())
    ));
    let retired = PathBuf::from(name);
    std::fs::rename(path, &retired)?;
    Ok(retired)
}

fn error(error: CapsuleStoreError) -> StorageError {
    match error {
        CapsuleStoreError::Conflict => StorageError::conflict("revision or unique conflict"),
        CapsuleStoreError::Failed(message) if message.starts_with("invalid") => {
            StorageError::Invalid(message)
        }
        CapsuleStoreError::Failed(message) if message.starts_with("unsupported") => {
            StorageError::Unsupported(message)
        }
        CapsuleStoreError::Failed(message) => StorageError::Unavailable(message),
    }
}

/// The secondary-index keys a live capsule holds (none for a tombstone or a kind the
/// catalog does not know).
pub(crate) fn index_keys(capsule: &CapsuleEnvelope) -> Result<BTreeSet<String>, CapsuleStoreError> {
    if capsule.tombstone {
        return Ok(BTreeSet::new());
    }
    let schema = Schema::catalog().map_err(invalid)?;
    let Ok(model) = schema.model(&capsule.kind.0) else {
        return Ok(BTreeSet::new());
    };
    let row = codec::decode(model, capsule).map_err(invalid)?;
    Ok(model
        .indexed()
        .into_iter()
        .filter_map(|field| model_index::index_key(model, field, row.get(field), &row.id))
        .collect())
}

fn capsule_id(id: Id) -> CapsuleId {
    CapsuleId(id.0)
}

/// The committed store, queried through its indexes.
struct Committed {
    store: Arc<RocksDbCapsuleStore>,
}

impl Committed {
    fn load(&self, model: &Model, id: Id) -> StorageResult<Option<CapsuleEnvelope>> {
        self.store
            .stored(&model.name, &capsule_id(id))
            .and_then(|stored| stored.envelope(&model.name, &capsule_id(id)))
            .map_err(error)
    }

    fn index_ids(&self, scan: &Scan) -> StorageResult<Vec<Id>> {
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        let keys = match scan {
            Scan::Prefixes(prefixes) => {
                let mut keys = Vec::new();
                for prefix in prefixes {
                    keys.extend(
                        self.store
                            .kv
                            .scan_prefix(prefix.as_bytes())
                            .map_err(|failure| error(storage(failure)))?,
                    );
                }
                keys
            }
            Scan::Range { start, end } => self
                .store
                .kv
                .scan_range(start.as_bytes(), end.as_bytes(), false, None)
                .map_err(|failure| error(storage(failure)))?,
        };
        for (key, _) in keys {
            if let Some(id) = model_index::key_id(&String::from_utf8_lossy(&key))
                && seen.insert(id)
            {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// Live rows matching `query`'s filter, ordered as the query asks; with an
    /// ordered plan, only the first `want` of them.
    fn rows(&self, model: &Model, query: &FindMany, want: u64) -> StorageResult<Vec<Row>> {
        let keep = |capsule: &CapsuleEnvelope| -> StorageResult<Option<Row>> {
            if capsule.tombstone {
                return Ok(None);
            }
            let row = codec::decode(model, capsule)?;
            Ok(query
                .filter
                .as_ref()
                .is_none_or(|filter| eval::matches(filter, &row))
                .then_some(row))
        };
        let mut rows = Vec::new();
        match model_index::plan(model, query) {
            Plan::Index { scan, .. } => {
                for id in self.index_ids(&scan)? {
                    if let Some(capsule) = self.load(model, id)?
                        && let Some(row) = keep(&capsule)?
                    {
                        rows.push(row);
                    }
                }
            }
            Plan::Ordered { field } => {
                let prefix = model_index::field_prefix(&model.name, &field);
                let keys = self
                    .store
                    .kv
                    .scan_range(
                        prefix.as_bytes(),
                        model_index::prefix_end(&prefix).as_bytes(),
                        false,
                        None,
                    )
                    .map_err(|failure| error(storage(failure)))?;
                for (key, _) in keys {
                    if rows.len() as u64 >= want {
                        return Ok(rows);
                    }
                    let Some(id) = model_index::key_id(&String::from_utf8_lossy(&key)) else {
                        continue;
                    };
                    if let Some(capsule) = self.load(model, id)?
                        && let Some(row) = keep(&capsule)?
                    {
                        rows.push(row);
                    }
                }
                // Rows without a value sort last; they come from a scan, in id order.
                let mut nulls = Vec::new();
                for capsule in self.store.scan_kind(&model.name).map_err(error)? {
                    if let Some(row) = keep(&capsule)?
                        && row.get(&field).is_null()
                    {
                        nulls.push(row);
                    }
                }
                nulls.sort_by_key(|left| left.id);
                rows.extend(nulls);
                return Ok(rows);
            }
            Plan::Scan => {
                for capsule in self.store.scan_kind(&model.name).map_err(error)? {
                    if let Some(row) = keep(&capsule)? {
                        rows.push(row);
                    }
                }
            }
        }
        rows.sort_by(|left, right| eval::order(left, right, &query.order_by));
        Ok(rows)
    }
}

/// The RocksDB provider: the model store under `storage_root/data`, local or
/// replicated by the provider's Raft cluster.
pub struct RocksDbProvider {
    store: Arc<RocksDbCapsuleStore>,
    logs: Arc<RocksDbConsensusLogStorage>,
    legacy_store: Option<PathBuf>,
    /// Where legacy consensus logs may remain (absent for a store opened by a tool).
    storage_root: Option<PathBuf>,
    serves_admin_routes: bool,
}

impl RocksDbProvider {
    /// A provider over an already-open key/value store (tests, tools), with the
    /// default memory budget.
    pub fn over(
        kv: Arc<dyn LegacyKvStore>,
        replicated: bool,
        logs_root: &Path,
    ) -> StorageResult<Self> {
        Ok(Self {
            store: Arc::new(RocksDbCapsuleStore::open(kv, replicated).map_err(error)?),
            logs: Arc::new(RocksDbConsensusLogStorage::new(
                logs_root,
                aseman_config::RocksDbTuning::default(),
            )),
            legacy_store: None,
            storage_root: None,
            serves_admin_routes: false,
        })
    }
}

pub struct RocksDbPlugin;

impl ProviderPlugin for RocksDbPlugin {
    fn name(&self) -> &'static str {
        NAME
    }

    fn open(&self, settings: &ProviderSettings) -> StorageResult<Arc<dyn StorageProvider>> {
        let path = settings.storage_root.join(DATA_DIRECTORY);
        std::fs::create_dir_all(&path).map_err(StorageError::unavailable)?;
        let local = Arc::new(
            RocksDbKvStore::open_tuned(&path, &settings.rocksdb)
                .map_err(StorageError::unavailable)?,
        );
        let routes = settings.admin_routes.clone();
        let serves_admin_routes = routes.is_some();
        let kv = cluster::open(
            &settings.storage_root,
            local,
            &settings.cluster,
            &settings.rocksdb,
            routes,
        )
        .map_err(StorageError::unavailable)?;
        let replicated = kv.cluster().is_some();
        let store = RocksDbCapsuleStore::open(Arc::new(kv), replicated).map_err(error)?;
        store.migrate_layout(settings.layout).map_err(error)?;
        Ok(Arc::new(RocksDbProvider {
            store: Arc::new(store),
            logs: Arc::new(RocksDbConsensusLogStorage::new(
                settings.storage_root.join(CONSENSUS_DIRECTORY),
                settings.rocksdb,
            )),
            legacy_store: settings.legacy_store.clone(),
            storage_root: Some(settings.storage_root.clone()),
            serves_admin_routes,
        }))
    }
}

impl StorageProvider for RocksDbProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn begin(&self, mode: Mode) -> StorageResult<Box<dyn CapsuleTransaction>> {
        Ok(Box::new(RocksDbTransaction {
            committed: Committed {
                store: self.store.clone(),
            },
            mode,
            writes: Mutex::new(Vec::new()),
        }))
    }

    fn consensus_logs(&self) -> Arc<dyn ConsensusLogStorage> {
        self.logs.clone()
    }

    fn export(
        &self,
        model: &Model,
        after: Option<Id>,
        limit: usize,
    ) -> StorageResult<Vec<CapsuleEnvelope>> {
        let mut capsules = self
            .store
            .scan_kind(&model.name)
            .map_err(error)?
            .into_iter()
            .filter(|capsule| after.is_none_or(|after| capsule.id.0 > after.0))
            .collect::<Vec<_>>();
        capsules.sort_by(|left, right| left.id.cmp(&right.id));
        capsules.truncate(limit);
        Ok(capsules)
    }

    fn import(&self, _model: &Model, capsules: &[CapsuleEnvelope]) -> StorageResult<()> {
        for capsule in capsules {
            capsule
                .verify()
                .map_err(|failure| StorageError::invalid(failure.to_string()))?;
        }
        if self.store.try_import(capsules).map_err(error)? {
            Ok(())
        } else {
            Err(StorageError::conflict(
                "the store changed during the import",
            ))
        }
    }

    fn serves_admin_routes(&self) -> bool {
        self.serves_admin_routes
    }

    fn legacy_layout(&self) -> StorageResult<Option<String>> {
        if let Some(path) = &self.legacy_store
            && path.join("CURRENT").is_file()
            && RocksDbKvStore::open_default(path)
                .and_then(|legacy| legacy.has_prefix(b""))
                .map_err(StorageError::unavailable)?
        {
            return Ok(Some(format!("legacy key/value ({})", path.display())));
        }
        if let Some(root) = &self.storage_root
            && let Some(log) = legacy_consensus_logs(root)
                .map_err(StorageError::unavailable)?
                .first()
        {
            return Ok(Some(format!(
                "legacy consensus log ({})",
                log.path.display()
            )));
        }
        Ok(None)
    }

    fn retire_legacy_layout(&self) -> StorageResult<()> {
        if let Some(path) = &self.legacy_store
            && path.exists()
        {
            retire_directory(path).map_err(StorageError::unavailable)?;
        }
        if let Some(root) = &self.storage_root {
            for log in legacy_consensus_logs(root).map_err(StorageError::unavailable)? {
                retire_directory(&log.path).map_err(StorageError::unavailable)?;
            }
        }
        Ok(())
    }
}

struct RocksDbTransaction {
    committed: Committed,
    mode: Mode,
    /// Every write in order: (kind, capsule, the revision it replaces).
    writes: Mutex<Vec<(String, CapsuleEnvelope, Option<u64>)>>,
}

fn poisoned<T>(_: T) -> StorageError {
    StorageError::unavailable("transaction lock poisoned")
}

impl RocksDbTransaction {
    /// This transaction's latest write of each record of `model`.
    fn overlay(&self, model: &Model) -> StorageResult<BTreeMap<Id, CapsuleEnvelope>> {
        Ok(self
            .writes
            .lock()
            .map_err(poisoned)?
            .iter()
            .filter(|(kind, _, _)| kind == &model.name)
            .map(|(_, capsule, _)| (Id(capsule.id.0), capsule.clone()))
            .collect())
    }

    fn live_row(model: &Model, capsule: &CapsuleEnvelope) -> StorageResult<Option<Row>> {
        if capsule.tombstone {
            return Ok(None);
        }
        codec::decode(model, capsule).map(Some)
    }

    /// Refuse a capsule whose unique-index values another live record holds.
    fn check_unique(&self, model: &Model, capsule: &CapsuleEnvelope) -> StorageResult<()> {
        let Some(row) = Self::live_row(model, capsule)? else {
            return Ok(());
        };
        for fields in &model.unique {
            if fields.iter().any(|field| row.get(field).is_null()) {
                continue;
            }
            let filter = Where::all(
                fields
                    .iter()
                    .map(|field| Where::eq(field, row.get(field).clone()))
                    .collect(),
            );
            let clash = self
                .find(
                    model,
                    &FindMany {
                        filter,
                        take: Some(2),
                        ..FindMany::default()
                    },
                )?
                .into_iter()
                .any(|other| other.id != capsule.id);
            if clash {
                return Err(StorageError::conflict(format!(
                    "{}: ({}) is taken",
                    model.name,
                    fields.join(", ")
                )));
            }
        }
        Ok(())
    }
}

impl CapsuleTransaction for RocksDbTransaction {
    fn get(&self, model: &Model, id: Id) -> StorageResult<Option<CapsuleEnvelope>> {
        if let Some(capsule) = self.overlay(model)?.remove(&id) {
            return Ok(Some(capsule));
        }
        self.committed.load(model, id)
    }

    fn find(&self, model: &Model, query: &FindMany) -> StorageResult<Vec<CapsuleEnvelope>> {
        let overlay = self.overlay(model)?;
        let take = query.take.unwrap_or(DEFAULT_TAKE).min(DEFAULT_TAKE);
        // The overlay can hide at most one committed row per record it holds.
        let want = query
            .skip
            .saturating_add(take)
            .saturating_add(overlay.len() as u64);
        let mut rows = self
            .committed
            .rows(model, query, want)?
            .into_iter()
            .filter(|row| !overlay.contains_key(&row.id))
            .collect::<Vec<_>>();
        for capsule in overlay.values() {
            if let Some(row) = Self::live_row(model, capsule)?
                && query
                    .filter
                    .as_ref()
                    .is_none_or(|filter| eval::matches(filter, &row))
            {
                rows.push(row);
            }
        }
        rows.sort_by(|left, right| eval::order(left, right, &query.order_by));
        let page = rows
            .into_iter()
            .skip(usize::try_from(query.skip).unwrap_or(usize::MAX))
            .take(usize::try_from(take).unwrap_or(usize::MAX))
            .map(|row| row.id)
            .collect::<Vec<_>>();
        let mut capsules = Vec::with_capacity(page.len());
        for id in page {
            if let Some(capsule) = self.get(model, id)? {
                capsules.push(capsule);
            }
        }
        Ok(capsules)
    }

    fn count(&self, model: &Model, filter: Option<&Where>) -> StorageResult<u64> {
        let overlay = self.overlay(model)?;
        let query = FindMany {
            filter: filter.cloned(),
            ..FindMany::default()
        };
        let committed = self
            .committed
            .rows(model, &query, u64::MAX)?
            .into_iter()
            .filter(|row| !overlay.contains_key(&row.id))
            .count() as u64;
        let mut written = 0;
        for capsule in overlay.values() {
            if let Some(row) = Self::live_row(model, capsule)?
                && filter.is_none_or(|filter| eval::matches(filter, &row))
            {
                written += 1;
            }
        }
        Ok(committed + written)
    }

    fn put(
        &self,
        model: &Model,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> StorageResult<()> {
        if self.mode == Mode::ReadOnly {
            return Err(StorageError::invalid("read-only transaction"));
        }
        capsule
            .verify()
            .map_err(|failure| StorageError::invalid(failure.to_string()))?;
        let current = self
            .get(model, Id(capsule.id.0))?
            .map(|stored| stored.revision);
        if current != expected_revision {
            return Err(StorageError::conflict(format!(
                "{}: stale revision",
                model.name
            )));
        }
        self.check_unique(model, capsule)?;
        self.writes.lock().map_err(poisoned)?.push((
            model.name.clone(),
            capsule.clone(),
            expected_revision,
        ));
        Ok(())
    }

    fn commit(&self) -> StorageResult<()> {
        let writes = std::mem::take(&mut *self.writes.lock().map_err(poisoned)?)
            .into_iter()
            .map(|(_, capsule, expected)| (capsule, expected))
            .collect::<Vec<_>>();
        if writes.is_empty() {
            return Ok(());
        }
        if self.committed.store.try_put_all(&writes).map_err(error)? {
            Ok(())
        } else {
            Err(StorageError::conflict(
                "another transaction changed these records",
            ))
        }
    }

    fn rollback(&self) -> StorageResult<()> {
        self.writes.lock().map_err(poisoned)?.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_storage::Storage;

    #[test]
    fn rocksdb_passes_the_storage_provider_conformance_suite() {
        let root = std::env::temp_dir().join(format!(
            "aseman-rocksdb-models-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut settings = ProviderSettings::embedded(&root).unwrap();
        settings.cluster = aseman_config::ClusterBootstrapConfig::default();
        let provider = RocksDbPlugin.open(&settings).unwrap();
        let storage = Storage::new(provider, Schema::catalog().unwrap());
        aseman_storage::conformance::storage_provider(&storage);
        let _ = std::fs::remove_dir_all(root);
    }
}
