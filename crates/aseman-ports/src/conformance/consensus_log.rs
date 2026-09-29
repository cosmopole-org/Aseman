//! The [`ConsensusLogStorage`] contract, and an in-memory reference implementation.

use crate::consensus_log::{ConsensusLog, ConsensusLogStorage, ConsensusLogWrite};
use crate::{PortError, PortResult};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

fn put(key: &str, value: &str) -> ConsensusLogWrite {
    ConsensusLogWrite::Put {
        key: key.as_bytes().to_vec(),
        value: value.as_bytes().to_vec(),
    }
}

/// Exercises a [`ConsensusLogStorage`]. `name` must be unused by the caller's other
/// tests; it is opened fresh.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn consensus_log(storage: &dyn ConsensusLogStorage, name: &str) {
    let log = storage.open(name, true).unwrap();
    assert_eq!(log.get(b"missing").unwrap(), None);

    // Ordered scans over byte keys, bounded by the prefix.
    log.write(&[
        put("round_000000002", "r2"),
        put("round_000000010", "r10"),
        put("round_000000001", "r1"),
        put("rounds", "not a round"),
        put("block_000000001", "b1"),
    ])
    .unwrap();
    let rounds = log.scan_prefix(b"round_").unwrap();
    assert_eq!(
        rounds
            .iter()
            .map(|(key, _)| String::from_utf8(key.clone()).unwrap())
            .collect::<Vec<_>>(),
        ["round_000000001", "round_000000002", "round_000000010"]
    );
    assert_eq!(log.get(b"block_000000001").unwrap(), Some(b"b1".to_vec()));

    // Binary keys and values survive exactly.
    let binary = vec![0_u8, 255, 1, 0];
    log.write(&[ConsensusLogWrite::Put {
        key: binary.clone(),
        value: binary.clone(),
    }])
    .unwrap();
    assert_eq!(log.get(&binary).unwrap(), Some(binary.clone()));

    // A batch applies in order: a later put wins over an earlier delete.
    log.write(&[
        ConsensusLogWrite::Delete {
            key: b"rounds".to_vec(),
        },
        ConsensusLogWrite::DeleteRange {
            start: b"round_000000000".to_vec(),
            end: b"round_000000003".to_vec(),
        },
        put("round_000000002", "again"),
    ])
    .unwrap();
    assert_eq!(log.get(b"rounds").unwrap(), None);
    assert_eq!(log.get(b"round_000000001").unwrap(), None);
    assert_eq!(
        log.get(b"round_000000002").unwrap(),
        Some(b"again".to_vec())
    );
    assert_eq!(log.get(b"round_000000010").unwrap(), Some(b"r10".to_vec()));
    log.flush().unwrap();

    // Logs are isolated from each other, and reopening keeps the contents.
    let other = storage.open(&format!("{name}-other"), true).unwrap();
    assert_eq!(other.get(b"block_000000001").unwrap(), None);
    other.write(&[put("block_000000001", "elsewhere")]).unwrap();
    drop(log);
    let reopened = storage.open(name, false).unwrap();
    assert_eq!(
        reopened.get(b"block_000000001").unwrap(),
        Some(b"b1".to_vec())
    );
    drop(reopened);

    // Both logs are listed by name; nothing set aside is.
    let names = storage.names().unwrap();
    let other_name = format!("{name}-other");
    assert!(names.contains(&name.to_owned()) && names.contains(&other_name));
    assert!(names.iter().all(|listed| !listed.contains("--UTC--")));
    assert!(
        names.windows(2).all(|pair| pair[0] < pair[1]),
        "names are sorted"
    );

    // A fresh open starts empty.
    let fresh = storage.open(name, true).unwrap();
    assert_eq!(fresh.get(b"block_000000001").unwrap(), None);
    assert!(fresh.scan_prefix(b"").unwrap().is_empty());
    assert_eq!(
        other.get(b"block_000000001").unwrap(),
        Some(b"elsewhere".to_vec())
    );
}

type Space = Arc<Mutex<BTreeMap<Vec<u8>, Vec<u8>>>>;

/// An in-memory [`ConsensusLogStorage`] for engine tests.
#[derive(Default)]
pub struct MemoryConsensusLogStorage {
    logs: Mutex<HashMap<String, Space>>,
}

struct MemoryLog(Space);

impl ConsensusLogStorage for MemoryConsensusLogStorage {
    fn open(&self, name: &str, fresh: bool) -> PortResult<Arc<dyn ConsensusLog>> {
        let mut logs = self
            .logs
            .lock()
            .map_err(|_| PortError::Unavailable("memory log lock"))?;
        if fresh {
            logs.remove(name);
        }
        let space = logs.entry(name.to_owned()).or_default().clone();
        Ok(Arc::new(MemoryLog(space)))
    }

    fn names(&self) -> PortResult<Vec<String>> {
        let mut names = self
            .logs
            .lock()
            .map_err(|_| PortError::Unavailable("memory log lock"))?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        names.sort();
        Ok(names)
    }
}

impl ConsensusLog for MemoryLog {
    fn get(&self, key: &[u8]) -> PortResult<Option<Vec<u8>>> {
        let space = self
            .0
            .lock()
            .map_err(|_| PortError::Unavailable("memory log lock"))?;
        Ok(space.get(key).cloned())
    }

    fn scan_prefix(&self, prefix: &[u8]) -> PortResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let space = self
            .0
            .lock()
            .map_err(|_| PortError::Unavailable("memory log lock"))?;
        Ok(space
            .range(prefix.to_vec()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect())
    }

    fn write(&self, batch: &[ConsensusLogWrite]) -> PortResult<()> {
        let mut space = self
            .0
            .lock()
            .map_err(|_| PortError::Unavailable("memory log lock"))?;
        for write in batch {
            match write {
                ConsensusLogWrite::Put { key, value } => {
                    space.insert(key.clone(), value.clone());
                }
                ConsensusLogWrite::Delete { key } => {
                    space.remove(key);
                }
                ConsensusLogWrite::DeleteRange { start, end } => {
                    let doomed = space
                        .range(start.clone()..end.clone())
                        .map(|(key, _)| key.clone())
                        .collect::<Vec<_>>();
                    for key in doomed {
                        space.remove(&key);
                    }
                }
            }
        }
        Ok(())
    }

    fn flush(&self) -> PortResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_memory_reference_passes_its_own_contract() {
        super::consensus_log(&super::MemoryConsensusLogStorage::default(), "memory");
    }
}
