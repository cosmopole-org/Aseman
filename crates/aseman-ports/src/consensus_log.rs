//! Storage for a consensus engine's log (ADR 0035).
//!
//! A consensus engine persists its events, rounds, blocks, and frames through this
//! port and never through a database driver. The selected storage provider supplies
//! the implementation, so the engine runs unchanged on embedded RocksDB, on
//! PostgreSQL, or on any later provider that passes
//! `conformance::consensus_log`.
//!
//! The contract is an ordered byte key/value space per *log*: keys compare as bytes,
//! a batch applies atomically, and a log is identified by a name the engine chooses
//! (one per chain). Values are opaque to the provider.

use crate::PortResult;
use std::sync::Arc;

/// One mutation in an atomic batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConsensusLogWrite {
    Put {
        key: Vec<u8>,
        value: Vec<u8>,
    },
    Delete {
        key: Vec<u8>,
    },
    /// Delete every key in `start..end` (end exclusive).
    DeleteRange {
        start: Vec<u8>,
        end: Vec<u8>,
    },
}

/// One consensus log: an ordered byte key/value space.
pub trait ConsensusLog: Send + Sync {
    fn get(&self, key: &[u8]) -> PortResult<Option<Vec<u8>>>;
    /// Every pair whose key starts with `prefix`, in ascending key order.
    fn scan_prefix(&self, prefix: &[u8]) -> PortResult<Vec<(Vec<u8>, Vec<u8>)>>;
    /// Apply every write atomically and in order, or none.
    fn write(&self, batch: &[ConsensusLogWrite]) -> PortResult<()>;
    /// Make every applied write durable.
    fn flush(&self) -> PortResult<()>;
}

/// Opens consensus logs; supplied by the selected storage provider.
pub trait ConsensusLogStorage: Send + Sync {
    /// Open the log `name`, creating it when absent. With `fresh`, the log's current
    /// contents are first set aside (kept, under a new name, for inspection) and the
    /// log starts empty.
    fn open(&self, name: &str, fresh: bool) -> PortResult<Arc<dyn ConsensusLog>>;
}
