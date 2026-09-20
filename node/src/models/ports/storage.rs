//! Storage port — the persistence driver interface.
//!
//! The node uses RocksDB (transactional) for its key/value store and an
//! `r2d2`-pooled QuestDB (PostgreSQL-wire) client for its time-series store.

use std::sync::Arc;

use anyhow::Result;

use crate::models::packet::{BuildPacket, LogPacket, LogQuery};
use crate::models::transaction::ITrx;

/// Key/value database handle — RocksDB transactional DB.
pub type KvDb = Arc<rocksdb::TransactionDB>;

/// Time-series database handle — pooled QuestDB (PostgreSQL-wire) client.
pub type TsDb = r2d2::Pool<r2d2_postgres::PostgresConnectionManager<postgres::tls::NoTls>>;

/// The storage driver interface.
pub trait IStorage: Send + Sync {
    fn storage_root(&self) -> String;
    fn kv_db(&self) -> KvDb;
    fn ts_db(&self) -> TsDb;
    fn gen_id(&self, t: &dyn ITrx, origin: &str) -> String;
    /// Append one signal packet to the store's time-series log. `tags` are the
    /// sender's labels, already validated by the caller; they are stored with
    /// the packet so [`IStorage::read_store_logs`] can filter on them.
    ///
    /// Errors rather than reporting a packet it did not write: this row is the
    /// message, so a caller must be able to tell the sender their message did
    /// not land instead of watching it vanish on the next read.
    fn log_time_sieries(
        &self,
        store_id: &str,
        user_id: &str,
        data: &str,
        tags: &[String],
        time_val: i64,
    ) -> Result<LogPacket>;
    fn update_log(
        &self,
        store_id: &str,
        user_id: &str,
        signal_id: &str,
        data: &str,
        time_val: i64,
    ) -> LogPacket;
    /// Read a store's persisted signals, newest first, filtered by the
    /// query's tags and time bounds.
    ///
    /// Errors rather than returning an empty page: "the log is unreachable" and
    /// "this store has nothing to say" must not look the same to a reader.
    fn read_store_logs(&self, store_id: &str, query: &LogQuery) -> Result<Vec<LogPacket>>;
    fn pick_store_logs(&self, store_id: &str, ids: Vec<String>) -> Vec<LogPacket>;
    fn log_vm(&self, vm_id: &str, log_type: &str, data: &str, time_val: i64) -> BuildPacket;
    fn read_vm_logs(
        &self,
        vm_id: &str,
        log_type: &str,
        offset: i64,
        count: i64,
    ) -> Vec<BuildPacket>;
}
