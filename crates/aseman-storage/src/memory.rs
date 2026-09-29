//! The in-memory reference provider: exact semantics, no durability. Tests and
//! ephemeral nodes use it, and the conformance suite is checked against it.

use crate::codec;
use crate::error::{StorageError, StorageResult};
use crate::eval;
use crate::provider::{
    CapsuleTransaction, Mode, ProviderPlugin, ProviderSettings, StorageProvider,
};
use crate::query::{FindMany, Where};
use crate::schema::Model;
use crate::value::{Id, Row, Value};
use aseman_contracts::capsule::CapsuleEnvelope;
use aseman_ports::consensus_log::{ConsensusLog, ConsensusLogStorage, ConsensusLogWrite};
use aseman_ports::{PortError, PortResult};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

type Key = (String, Id);
type Capsules = BTreeMap<Key, CapsuleEnvelope>;

/// Rows a query without `take` returns at most.
pub const DEFAULT_TAKE: u64 = 10_000;

#[derive(Default)]
pub struct MemoryProvider {
    committed: Arc<Mutex<Capsules>>,
    logs: Arc<MemoryLogs>,
}

impl MemoryProvider {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

fn poisoned<T>(_: T) -> StorageError {
    StorageError::unavailable("memory store lock poisoned")
}

impl StorageProvider for MemoryProvider {
    fn name(&self) -> &str {
        "memory"
    }

    fn begin(&self, mode: Mode) -> StorageResult<Box<dyn CapsuleTransaction>> {
        Ok(Box::new(MemoryTransaction {
            committed: self.committed.clone(),
            writes: Mutex::new(Vec::new()),
            mode,
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
        let committed = self.committed.lock().map_err(poisoned)?;
        Ok(committed
            .iter()
            .filter(|((kind, id), _)| kind == &model.name && after.is_none_or(|after| *id > after))
            .take(limit)
            .map(|(_, capsule)| capsule.clone())
            .collect())
    }

    fn import(&self, model: &Model, capsules: &[CapsuleEnvelope]) -> StorageResult<()> {
        let mut committed = self.committed.lock().map_err(poisoned)?;
        for capsule in capsules {
            capsule
                .verify()
                .map_err(|error| StorageError::invalid(error.to_string()))?;
            let key = (model.name.clone(), Id(capsule.id.0));
            match committed.get(&key) {
                Some(stored) if stored == capsule => {}
                Some(stored) if stored.revision >= capsule.revision => {
                    return Err(StorageError::conflict(format!(
                        "{}: {} holds another revision",
                        model.name, key.1
                    )));
                }
                _ => {
                    committed.insert(key, capsule.clone());
                }
            }
        }
        Ok(())
    }
}

/// A plugin that hands out one shared memory provider (every `open` sees the same
/// data), for tests and ephemeral nodes.
pub struct MemoryPlugin(pub Arc<MemoryProvider>);

impl ProviderPlugin for MemoryPlugin {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn open(&self, _: &ProviderSettings) -> StorageResult<Arc<dyn StorageProvider>> {
        Ok(self.0.clone())
    }
}

struct MemoryTransaction {
    committed: Arc<Mutex<Capsules>>,
    /// Buffered writes: (capsule, the revision it expects).
    writes: Mutex<Vec<(String, CapsuleEnvelope, Option<u64>)>>,
    mode: Mode,
}

impl MemoryTransaction {
    /// The committed capsules of `model` with this transaction's writes applied.
    fn view(&self, model: &Model) -> StorageResult<BTreeMap<Id, CapsuleEnvelope>> {
        let committed = self.committed.lock().map_err(poisoned)?;
        let mut view = committed
            .iter()
            .filter(|((kind, _), _)| kind == &model.name)
            .map(|((_, id), capsule)| (*id, capsule.clone()))
            .collect::<BTreeMap<_, _>>();
        for (kind, capsule, _) in self.writes.lock().map_err(poisoned)?.iter() {
            if kind == &model.name {
                view.insert(Id(capsule.id.0), capsule.clone());
            }
        }
        Ok(view)
    }

    fn rows(&self, model: &Model, filter: Option<&Where>) -> StorageResult<Vec<Row>> {
        let mut rows = Vec::new();
        for capsule in self.view(model)?.values() {
            if capsule.tombstone {
                continue;
            }
            let row = codec::decode(model, capsule)?;
            if filter.is_none_or(|filter| eval::matches(filter, &row)) {
                rows.push(row);
            }
        }
        Ok(rows)
    }
}

/// The unique-index values `row` claims (an index with a null value claims nothing).
pub fn unique_claims(model: &Model, row: &Row) -> Vec<(usize, Vec<Value>)> {
    model
        .unique
        .iter()
        .enumerate()
        .filter_map(|(index, fields)| {
            let values = fields
                .iter()
                .map(|field| row.get(field).clone())
                .collect::<Vec<_>>();
            (!values.iter().any(Value::is_null)).then_some((index, values))
        })
        .collect()
}

impl CapsuleTransaction for MemoryTransaction {
    fn get(&self, model: &Model, id: Id) -> StorageResult<Option<CapsuleEnvelope>> {
        Ok(self.view(model)?.remove(&id))
    }

    fn find(&self, model: &Model, query: &FindMany) -> StorageResult<Vec<CapsuleEnvelope>> {
        let mut rows = self.rows(model, query.filter.as_ref())?;
        rows.sort_by(|left, right| eval::order(left, right, &query.order_by));
        let view = self.view(model)?;
        Ok(rows
            .into_iter()
            .skip(usize::try_from(query.skip).unwrap_or(usize::MAX))
            .take(usize::try_from(query.take.unwrap_or(DEFAULT_TAKE)).unwrap_or(usize::MAX))
            .filter_map(|row| view.get(&row.id).cloned())
            .collect())
    }

    fn count(&self, model: &Model, filter: Option<&Where>) -> StorageResult<u64> {
        Ok(self.rows(model, filter)?.len() as u64)
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
            .map_err(|error| StorageError::invalid(error.to_string()))?;
        let id = Id(capsule.id.0);
        let current = self.view(model)?.get(&id).map(|stored| stored.revision);
        if current != expected_revision {
            return Err(StorageError::conflict(format!(
                "{}: stale revision",
                model.name
            )));
        }
        if !capsule.tombstone {
            let row = codec::decode(model, capsule)?;
            let claims = unique_claims(model, &row);
            for other in self.rows(model, None)? {
                if other.id != id
                    && unique_claims(model, &other)
                        .iter()
                        .any(|claim| claims.contains(claim))
                {
                    return Err(StorageError::conflict(format!(
                        "{}: a unique index already holds these values",
                        model.name
                    )));
                }
            }
        }
        self.writes.lock().map_err(poisoned)?.push((
            model.name.clone(),
            capsule.clone(),
            expected_revision,
        ));
        Ok(())
    }

    fn commit(&self) -> StorageResult<()> {
        let writes = std::mem::take(&mut *self.writes.lock().map_err(poisoned)?);
        let mut committed = self.committed.lock().map_err(poisoned)?;
        // Check every write against the committed state first: all apply, or none.
        let mut revisions: HashMap<Key, Option<u64>> = HashMap::new();
        for (kind, capsule, expected) in &writes {
            let key = (kind.clone(), Id(capsule.id.0));
            let current = revisions
                .get(&key)
                .copied()
                .unwrap_or_else(|| committed.get(&key).map(|stored| stored.revision));
            if current != *expected {
                return Err(StorageError::conflict(format!(
                    "{kind}: another transaction changed {}",
                    key.1
                )));
            }
            revisions.insert(key, Some(capsule.revision));
        }
        let mut next = committed.clone();
        for (kind, capsule, _) in writes {
            next.insert((kind, Id(capsule.id.0)), capsule);
        }
        // Unique indexes hold over the resulting state.
        let schema = crate::schema::Schema::catalog()?;
        let mut claims = std::collections::HashSet::new();
        for ((kind, id), capsule) in &next {
            if capsule.tombstone {
                continue;
            }
            let Ok(model) = schema.model(kind) else {
                continue;
            };
            let row = codec::decode(model, capsule)?;
            for (index, values) in unique_claims(model, &row) {
                if !claims.insert((kind.clone(), index, format!("{values:?}"))) {
                    return Err(StorageError::conflict(format!(
                        "{kind}: {id} collides on a unique index"
                    )));
                }
            }
        }
        *committed = next;
        Ok(())
    }

    fn rollback(&self) -> StorageResult<()> {
        self.writes.lock().map_err(poisoned)?.clear();
        Ok(())
    }
}

/// One log's ordered entries.
type LogEntries = Arc<Mutex<BTreeMap<Vec<u8>, Vec<u8>>>>;

/// In-memory consensus logs.
#[derive(Default)]
pub struct MemoryLogs {
    logs: Mutex<HashMap<String, LogEntries>>,
}

struct MemoryLog(LogEntries);

fn log_poisoned<T>(_: T) -> PortError {
    PortError::Unavailable("memory log lock poisoned")
}

impl ConsensusLogStorage for MemoryLogs {
    fn open(&self, name: &str, fresh: bool) -> PortResult<Arc<dyn ConsensusLog>> {
        let mut logs = self.logs.lock().map_err(log_poisoned)?;
        if fresh {
            logs.remove(name);
        }
        Ok(Arc::new(MemoryLog(
            logs.entry(name.to_owned()).or_default().clone(),
        )))
    }

    fn names(&self) -> PortResult<Vec<String>> {
        let mut names = self
            .logs
            .lock()
            .map_err(log_poisoned)?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        names.sort();
        Ok(names)
    }
}

impl ConsensusLog for MemoryLog {
    fn get(&self, key: &[u8]) -> PortResult<Option<Vec<u8>>> {
        Ok(self.0.lock().map_err(log_poisoned)?.get(key).cloned())
    }

    fn scan_prefix(&self, prefix: &[u8]) -> PortResult<Vec<(Vec<u8>, Vec<u8>)>> {
        Ok(self
            .0
            .lock()
            .map_err(log_poisoned)?
            .range(prefix.to_vec()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect())
    }

    fn write(&self, batch: &[ConsensusLogWrite]) -> PortResult<()> {
        let mut log = self.0.lock().map_err(log_poisoned)?;
        for write in batch {
            match write {
                ConsensusLogWrite::Put { key, value } => {
                    log.insert(key.clone(), value.clone());
                }
                ConsensusLogWrite::Delete { key } => {
                    log.remove(key);
                }
                ConsensusLogWrite::DeleteRange { start, end } => {
                    let doomed = log
                        .range(start.clone()..end.clone())
                        .map(|(key, _)| key.clone())
                        .collect::<Vec<_>>();
                    for key in doomed {
                        log.remove(&key);
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
