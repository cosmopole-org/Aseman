//! The legacy key/value store behind a provider-owned seam. The node's transaction
//! layer depends only on [`LegacyKvStore`]; RocksDB types stay inside this module.

use super::*;
use rocksdb::{Direction, TransactionDB, TransactionDBOptions, WriteBatchWithTransaction};

/// One write in an atomic batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegacyKvWrite {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

/// Behavioral requirements the legacy node needs from its key/value store.
pub trait LegacyKvStore: Send + Sync {
    fn get(&self, key: &[u8]) -> LegacyMigrationResult<Option<Vec<u8>>>;
    /// Every pair whose key starts with `prefix`, in ascending key order.
    fn scan_prefix(&self, prefix: &[u8]) -> LegacyMigrationResult<Vec<(Vec<u8>, Vec<u8>)>>;
    /// Every pair in ascending key order (recovery and diagnostics only).
    fn scan_all(&self) -> LegacyMigrationResult<Vec<(Vec<u8>, Vec<u8>)>>;
    /// Apply every write atomically, or none.
    fn write_batch(&self, writes: &[LegacyKvWrite]) -> LegacyMigrationResult<()>;
}

/// The legacy application store: a RocksDB `TransactionDB`.
pub struct RocksDbKvStore {
    db: TransactionDB,
}

fn storage_error(error: rocksdb::Error) -> LegacyMigrationError {
    LegacyMigrationError::Storage(error.to_string())
}

impl RocksDbKvStore {
    /// Open (creating if missing) with the shared bounded-memory tuning.
    pub fn open_tuned(path: &Path) -> LegacyMigrationResult<Self> {
        let mut options = crate::tuning::tuned_options();
        options.create_if_missing(true);
        TransactionDB::open(&options, &TransactionDBOptions::default(), path)
            .map(|db| Self { db })
            .map_err(storage_error)
    }

    /// Open with default options (tests and tools).
    pub fn open_default(path: &Path) -> LegacyMigrationResult<Self> {
        TransactionDB::open_default(path)
            .map(|db| Self { db })
            .map_err(storage_error)
    }
}

impl LegacyKvStore for RocksDbKvStore {
    fn get(&self, key: &[u8]) -> LegacyMigrationResult<Option<Vec<u8>>> {
        self.db.get(key).map_err(storage_error)
    }

    fn scan_prefix(&self, prefix: &[u8]) -> LegacyMigrationResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut pairs = Vec::new();
        // Seek to the prefix and stop at the first key outside it.
        for item in self
            .db
            .iterator(IteratorMode::From(prefix, Direction::Forward))
        {
            let (key, value) = item.map_err(storage_error)?;
            if !key.starts_with(prefix) {
                break;
            }
            pairs.push((key.to_vec(), value.to_vec()));
        }
        Ok(pairs)
    }

    fn scan_all(&self) -> LegacyMigrationResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.db
            .iterator(IteratorMode::Start)
            .map(|item| {
                item.map(|(key, value)| (key.to_vec(), value.to_vec()))
                    .map_err(storage_error)
            })
            .collect()
    }

    fn write_batch(&self, writes: &[LegacyKvWrite]) -> LegacyMigrationResult<()> {
        let mut batch = WriteBatchWithTransaction::<true>::default();
        for write in writes {
            match write {
                LegacyKvWrite::Put { key, value } => batch.put(key, value),
                LegacyKvWrite::Delete { key } => batch.delete(key),
            }
        }
        self.db.write(batch).map_err(storage_error)
    }
}
