//! The RocksDB provider's consensus-log storage (ADR 0035, ADR 0036).
//!
//! Each consensus log is its own embedded RocksDB database, opened with the shared
//! bounded-memory tuning, in the directory its relative name names under the log
//! root (`{storage_root}/consensus`). Setting a log aside renames its directory to
//! `<name>--UTC--<timestamp>`, the naming the engine always used.

use crate::tuning::tuned_options;
use aseman_config::RocksDbTuning;
use aseman_ports::consensus_log::{ConsensusLog, ConsensusLogStorage, ConsensusLogWrite};
use aseman_ports::{PortError, PortResult};
use rocksdb::{DB, Direction, IteratorMode, WriteBatch};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

fn failed(error: impl std::fmt::Display) -> PortError {
    PortError::Failed(format!("consensus log storage: {error}"))
}

/// Consensus logs as embedded RocksDB databases under `root`.
pub struct RocksDbConsensusLogStorage {
    root: PathBuf,
    tuning: RocksDbTuning,
    /// A RocksDB directory opens once per process: a log still held by the engine is
    /// handed out again instead of being reopened.
    open: Mutex<HashMap<PathBuf, Weak<RocksDbConsensusLog>>>,
}

impl RocksDbConsensusLogStorage {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, tuning: RocksDbTuning) -> Self {
        Self {
            root: root.into(),
            tuning,
            open: Mutex::new(HashMap::new()),
        }
    }
}

/// `<base>--UTC--<YYYY-MM-DDThh-mm-ss.nnnnnnnnnZ>`, path-safe.
fn archive_name(base: &std::path::Path) -> PathBuf {
    let now = chrono::Utc::now();
    let mut name = base.as_os_str().to_owned();
    name.push(format!("--UTC--{}", now.format("%Y-%m-%dT%H-%M-%S.%9fZ")));
    PathBuf::from(name)
}

impl ConsensusLogStorage for RocksDbConsensusLogStorage {
    fn open(&self, name: &str, fresh: bool) -> PortResult<Arc<dyn ConsensusLog>> {
        if !valid_name(name) {
            return Err(failed(format!("{name:?} is not a relative log name")));
        }
        let path = self.root.join(name);
        let mut open = self.open.lock().map_err(|_| failed("lock poisoned"))?;
        if let Some(log) = open.get(&path).and_then(Weak::upgrade) {
            if fresh {
                return Err(failed(format!(
                    "{} is still open and cannot start fresh",
                    path.display()
                )));
            }
            return Ok(log);
        }
        if fresh {
            match std::fs::rename(&path, archive_name(&path)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(failed(format!("set {} aside: {error}", path.display()))),
            }
        }
        let mut options = tuned_options(&self.tuning);
        options.create_if_missing(true);
        let db = DB::open(&options, &path).map_err(failed)?;
        let log = Arc::new(RocksDbConsensusLog { db });
        open.insert(path, Arc::downgrade(&log));
        Ok(log)
    }

    fn names(&self) -> PortResult<Vec<String>> {
        let mut names = Vec::new();
        collect_names(&self.root, &self.root, &mut names)?;
        names.sort();
        Ok(names)
    }
}

/// Whether `name` is a relative path of plain components.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && std::path::Path::new(name)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

/// Every log directory under `directory` (one holding a RocksDB `CURRENT` file),
/// as names relative to `root`; set-aside logs are skipped.
fn collect_names(root: &Path, directory: &Path, names: &mut Vec<String>) -> PortResult<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(failed(error)),
    };
    for entry in entries {
        let path = entry.map_err(failed)?.path();
        if !path.is_dir() || path.to_string_lossy().contains("--UTC--") {
            continue;
        }
        if path.join("CURRENT").is_file() {
            let name = path.strip_prefix(root).map_err(failed)?;
            names.push(name.to_string_lossy().into_owned());
        } else {
            collect_names(root, &path, names)?;
        }
    }
    Ok(())
}

pub struct RocksDbConsensusLog {
    db: DB,
}

impl ConsensusLog for RocksDbConsensusLog {
    fn get(&self, key: &[u8]) -> PortResult<Option<Vec<u8>>> {
        self.db.get(key).map_err(failed)
    }

    fn scan_prefix(&self, prefix: &[u8]) -> PortResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut pairs = Vec::new();
        for item in self
            .db
            .iterator(IteratorMode::From(prefix, Direction::Forward))
        {
            let (key, value) = item.map_err(failed)?;
            if !key.starts_with(prefix) {
                break;
            }
            pairs.push((key.to_vec(), value.to_vec()));
        }
        Ok(pairs)
    }

    fn write(&self, batch: &[ConsensusLogWrite]) -> PortResult<()> {
        let mut writes = WriteBatch::default();
        for write in batch {
            match write {
                ConsensusLogWrite::Put { key, value } => writes.put(key, value),
                ConsensusLogWrite::Delete { key } => writes.delete(key),
                ConsensusLogWrite::DeleteRange { start, end } => writes.delete_range(start, end),
            }
        }
        self.db.write(writes).map_err(failed)
    }

    fn flush(&self) -> PortResult<()> {
        self.db.flush().map_err(failed)
    }
}

#[cfg(test)]
mod tests {
    use aseman_ports::consensus_log::ConsensusLogStorage as _;

    #[test]
    fn rocksdb_logs_pass_the_consensus_log_contract() {
        let root = std::env::temp_dir().join(format!(
            "aseman-consensus-log-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let storage =
            super::RocksDbConsensusLogStorage::new(&root, aseman_config::RocksDbTuning::default());
        aseman_ports::conformance::consensus_log::consensus_log(&storage, "main/rocksdb_db");
        assert!(storage.open("/absolute", false).is_err());
        assert!(storage.open("../escape", false).is_err());
        // The first log's contents were set aside, not destroyed.
        let archived = std::fs::read_dir(root.join("main"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("rocksdb_db--UTC--")
            })
            .count();
        assert_eq!(archived, 1);
        let _ = std::fs::remove_dir_all(root);
    }
}
