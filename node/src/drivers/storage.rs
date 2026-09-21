//! Translation of `drivers/storage/storage.go`.
//!
//! `Storage` implements [`IStorage`]: the key/value database is RocksDB
//! (via `rocksdb::TransactionDB`, the same family used by the hashgraph
//! store); the time-series database is QuestDB exposed through its PG-wire
//! interface, accessed via an `r2d2`-pooled `postgres` client. The Go
//! original spun on connect for QuestDB until the schema was available; the
//! translation preserves that behaviour for the `storage` table.

use std::fs;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use aseman_storage_legacy::{LegacyBuildLogRow, LegacySignalRow, QuestDbTimeSeries};
use uuid::Uuid;

use crate::models::core::ICore;
use crate::models::packet::signal_tags::{decode_tags, encode_tags};
use crate::models::packet::{BuildPacket, LogPacket, LogQuery};
use crate::models::ports::storage::{IStorage, KvDb};
use crate::models::transaction::ITrx;

/// Concrete [`IStorage`] implementation.
pub struct Storage {
    _app: Arc<dyn ICore>,
    storage_root: String,
    kvdb: KvDb,
    tsdb: QuestDbTimeSeries,
    lock: Mutex<()>,
}

impl Storage {
    /// `NewStorage(core, storageRoot, baseDbPath, _logsDbPath,
    /// _searcherDbPath)`. The trailing arguments are kept for parity but the
    /// log/searcher tables live inside the QuestDB instance, not on disk.
    pub fn new(
        app: Arc<dyn ICore>,
        storage_root: &str,
        base_db_path: &str,
        _logs_db_path: &str,
        _searcher_db_path: &str,
        questdb_port: u16,
    ) -> Result<Arc<Storage>> {
        fs::create_dir_all(base_db_path).map_err(|e| anyhow!("mkdir {}: {}", base_db_path, e))?;
        // Bounded-memory options instead of `open_default`: this DB takes a
        // write per signal and is never fully pruned, so the default unlimited
        // `max_open_files` grew resident memory without bound as SST files
        // accumulated. See `aseman_storage_legacy::tuning`.
        let kvdb: KvDb = Arc::new(
            aseman_storage_legacy::RocksDbKvStore::open_tuned(std::path::Path::new(base_db_path))
                .map_err(|e| anyhow!("open kvdb {}: {}", base_db_path, e))?,
        );

        // The QuestDB client, its startup table repair, and its SQL live in the
        // legacy storage provider; this driver never names QuestDB types.
        let tsdb = QuestDbTimeSeries::connect(questdb_port).map_err(|e| anyhow!("{e}"))?;

        Ok(Arc::new(Storage {
            _app: app,
            storage_root: storage_root.to_string(),
            kvdb,
            tsdb,
            lock: Mutex::new(()),
        }))
    }
}

impl IStorage for Storage {
    fn storage_root(&self) -> String {
        self.storage_root.clone()
    }

    fn kv_db(&self) -> KvDb {
        self.kvdb.clone()
    }


    fn gen_id(&self, t: &dyn ITrx, origin: &str) -> String {
        // This mutex exists ONLY to make the id-counter read-modify-write below
        // atomic across concurrent callers. It must NOT be taken by the QuestDB
        // (tsdb) log/read helpers: holding it across a blocking QuestDB round
        // trip serialises every id mint behind log I/O, so a log-flooding VM
        // could starve createMachine/createProgram into a request timeout.
        let _guard = self.lock.lock().unwrap();
        if origin == "global" {
            let bytes = t.get_bytes("globalIdCounter");
            let mut counter: i64 = if bytes.is_empty() {
                0
            } else if bytes.len() >= 8 {
                i64::from_be_bytes(bytes[..8].try_into().unwrap())
            } else {
                0
            };
            counter += 1;
            t.put_bytes("globalIdCounter", counter.to_be_bytes().to_vec());
            format!("{}@{}", counter, origin)
        } else {
            // Use the kvdb directly when origin != "global", matching Go.
            let key = b"localIdCounter";
            let counter = {
                let val = self.kvdb.get(key).ok().flatten().unwrap_or_default();
                let mut counter: i64 = if val.len() >= 8 {
                    i64::from_be_bytes(val[..8].try_into().unwrap())
                } else {
                    0
                };
                counter += 1;
                let _ = self.kvdb.write_batch(&[aseman_storage_legacy::LegacyKvWrite::Put {
                    key: key.to_vec(),
                    value: counter.to_be_bytes().to_vec(),
                }]);
                counter
            };
            format!("{}@{}", counter, origin)
        }
    }

    fn log_time_sieries(
        &self,
        store_id: &str,
        user_id: &str,
        data: &str,
        tags: &[String],
        time_val: i64,
    ) -> Result<LogPacket> {
        // A failed insert is returned, never swallowed: this row IS the message.
        let row = LegacySignalRow {
            id: Uuid::new_v4().to_string(),
            store_id: store_id.to_string(),
            user_id: user_id.to_string(),
            data: data.to_string(),
            encoded_tags: encode_tags(tags),
            time_millis: time_val,
            edited: false,
        };
        self.tsdb.insert_signal(&row).map_err(|e| anyhow!("{e}"))?;
        Ok(LogPacket {
            id: row.id,
            user_id: row.user_id,
            data: row.data,
            store_id: row.store_id,
            tags: tags.to_vec(),
            time: time_val,
            edited: false,
        })
    }

    fn update_log(
        &self,
        store_id: &str,
        user_id: &str,
        signal_id: &str,
        data: &str,
        time_val: i64,
    ) -> LogPacket {
        self.tsdb.update_signal(store_id, signal_id, data);
        LogPacket {
            id: signal_id.to_string(),
            user_id: user_id.to_string(),
            data: data.to_string(),
            store_id: store_id.to_string(),
            // Tags are immutable: an edit rewrites the payload, never the labels
            // a reader filtered on to find the packet in the first place.
            tags: Vec::new(),
            time: time_val,
            edited: true,
        }
    }

    fn read_store_logs(&self, store_id: &str, query: &LogQuery) -> Result<Vec<LogPacket>> {
        // An unreachable or failing log is an ERROR, not an empty conversation.
        Ok(self
            .tsdb
            .read_signals(store_id, query)
            .map_err(|e| anyhow!("{e}"))?
            .into_iter()
            .map(log_packet)
            .collect())
    }

    fn pick_store_logs(&self, store_id: &str, ids: Vec<String>) -> Vec<LogPacket> {
        self.tsdb
            .pick_signals(store_id, &ids)
            .into_iter()
            .map(log_packet)
            .collect()
    }

    fn log_vm(&self, vm_id: &str, log_type: &str, data: &str, time_val: i64) -> BuildPacket {
        // No storage.lock: a running VM streams many log lines through here.
        let log_type = if log_type.is_empty() {
            "runtime"
        } else {
            log_type
        };
        let time_val = if time_val == 0 {
            chrono::Utc::now().timestamp_millis()
        } else {
            time_val
        };
        let row = LegacyBuildLogRow {
            id: Uuid::new_v4().to_string(),
            build_id: String::new(),
            machine_id: String::new(),
            vm_id: vm_id.to_string(),
            log_type: log_type.to_string(),
            data: data.to_string(),
            time_millis: time_val,
        };
        self.tsdb.insert_build_log(&row);
        build_packet(row)
    }

    fn read_vm_logs(
        &self,
        vm_id: &str,
        log_type: &str,
        offset: i64,
        count: i64,
    ) -> Vec<BuildPacket> {
        self.tsdb
            .read_build_logs(vm_id, log_type, offset, count)
            .into_iter()
            .map(build_packet)
            .collect()
    }
}

fn log_packet(row: LegacySignalRow) -> LogPacket {
    LogPacket {
        tags: decode_tags(&row.encoded_tags),
        id: row.id,
        user_id: row.user_id,
        data: row.data,
        store_id: row.store_id,
        time: row.time_millis,
        edited: row.edited,
    }
}

fn build_packet(row: LegacyBuildLogRow) -> BuildPacket {
    BuildPacket {
        id: row.id,
        build_id: row.build_id,
        creature_id: row.machine_id,
        vm_id: row.vm_id,
        log_type: row.log_type,
        time: row.time_millis,
        data: row.data,
    }
}
