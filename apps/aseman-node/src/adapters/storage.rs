//! The node's storage driver over the selected provider (ADR 0033).
//!
//! State transactions run on exactly one provider: the RocksDB provider's key/value
//! store (local, or replicated through its OpenRaft cluster) or the PostgreSQL
//! provider's compatibility transactions. The signal and build-log time series is
//! PostgreSQL or QuestDB (`ASEMAN_SIGNAL_LOG_PROVIDER`).

use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use aseman_storage_rocksdb::{LegacySignalRow, QuestDbTimeSeries};
use uuid::Uuid;

use crate::models::core::ICore;
use crate::models::packet::{LogPacket, LogQuery};
use crate::models::packet::{decode_tags, encode_tags};
use crate::models::ports::{IStorage, StateBackend};
use crate::models::transaction::ITrx;

/// Where the legacy signal and build-log tables are served from.
pub enum SignalLogTarget {
    /// The legacy QuestDB instance on this port.
    QuestDb(u16),
    /// PostgreSQL at this connection URL (`ASEMAN_SIGNAL_LOG_PROVIDER=postgres`).
    Postgres(String),
}

/// Concrete [`IStorage`] implementation.
pub struct Storage {
    _app: Arc<dyn ICore>,
    storage_root: String,
    state: StateBackend,
    tsdb: QuestDbTimeSeries,
    lock: Mutex<()>,
}

impl Storage {
    /// Compose the driver over an already-opened provider backend.
    pub fn new(
        app: Arc<dyn ICore>,
        storage_root: &str,
        state: StateBackend,
        signal_log: SignalLogTarget,
    ) -> Result<Arc<Storage>> {
        // The QuestDB client, its startup table repair, and its SQL live in the
        // legacy storage provider; this driver never names QuestDB types.
        let tsdb = match signal_log {
            SignalLogTarget::QuestDb(port) => QuestDbTimeSeries::connect(port),
            SignalLogTarget::Postgres(url) => QuestDbTimeSeries::connect_postgres(&url),
        }
        .map_err(|e| anyhow!("{e}"))?;

        Ok(Arc::new(Storage {
            _app: app,
            storage_root: storage_root.to_string(),
            state,
            tsdb,
            lock: Mutex::new(()),
        }))
    }
}

impl IStorage for Storage {
    fn storage_root(&self) -> String {
        self.storage_root.clone()
    }

    fn state(&self) -> StateBackend {
        self.state.clone()
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
            let key = "localIdCounter";
            let decode = |value: &[u8]| -> i64 {
                if value.len() >= 8 {
                    i64::from_be_bytes(value[..8].try_into().unwrap())
                } else {
                    0
                }
            };
            let counter = match &self.state {
                // RocksDB: the counter bypasses the transaction, as the original did.
                StateBackend::RocksDb(kvdb) => {
                    let counter =
                        decode(&kvdb.get(key.as_bytes()).ok().flatten().unwrap_or_default()) + 1;
                    let _ = kvdb.write_batch(&[aseman_storage_rocksdb::LegacyKvWrite::Put {
                        key: key.as_bytes().to_vec(),
                        value: counter.to_be_bytes().to_vec(),
                    }]);
                    counter
                }
                // PostgreSQL: the counter commits with the transaction that uses it.
                StateBackend::Postgres(_) => {
                    let counter = decode(&t.get_bytes(key)) + 1;
                    t.put_bytes(key, counter.to_be_bytes().to_vec());
                    counter
                }
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
