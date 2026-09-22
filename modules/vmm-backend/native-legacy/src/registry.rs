//! The workloads this backend runs, their logs and counters, and the plugin-local
//! state the runtime plugins keep (container names, module paths, provider handles).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use aseman_contracts::guest_api::WorkloadCredential;
use aseman_domain::vmm::{LogRecord, LogStream, Observation, Usage, WorkloadRecord};
use aseman_domain::{ObservedWorkloadState, WorkloadId};

/// The most log records kept per workload.
pub const LOG_CAPACITY: usize = 10_000;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One workload this backend runs.
pub struct Instance {
    pub record: WorkloadRecord,
    pub credential: Arc<WorkloadCredential>,
    /// The legacy machine (program) and VM identifiers the plugins address.
    pub machine_id: String,
    pub vm_id: String,
    pub entity_id: String,
    pub artifact_path: PathBuf,
    pub observation: Observation,
    pub logs: VecDeque<LogRecord>,
    pub next_log: u64,
    pub usage: Usage,
}

impl Instance {
    pub fn log(&mut self, stream: LogStream, line: &str, at_millis: i64) {
        self.next_log += 1;
        if self.logs.len() == LOG_CAPACITY {
            self.logs.pop_front();
        }
        self.logs.push_back(LogRecord {
            sequence: self.next_log,
            at_millis,
            stream,
            line: line.chars().take(65_536).collect(),
        });
    }
}

#[derive(Default)]
struct Inner {
    instances: BTreeMap<WorkloadId, Instance>,
    /// VM identifier → workload, for the plugins' callbacks (keyed by VM).
    by_vm: HashMap<String, WorkloadId>,
    sequence: u64,
}

/// Shared by the backend and its `VmHost`.
#[derive(Default, Clone)]
pub struct Registry {
    inner: Arc<Mutex<Inner>>,
}

impl Registry {
    /// The next observation sequence (backend-wide, increasing).
    pub fn next_sequence(&self) -> u64 {
        let mut inner = lock(&self.inner);
        inner.sequence += 1;
        inner.sequence
    }

    pub fn insert(&self, instance: Instance) {
        let mut inner = lock(&self.inner);
        inner
            .by_vm
            .insert(instance.vm_id.clone(), instance.record.id);
        inner.instances.insert(instance.record.id, instance);
    }

    pub fn remove(&self, id: WorkloadId) -> Option<Instance> {
        let mut inner = lock(&self.inner);
        let instance = inner.instances.remove(&id)?;
        inner.by_vm.remove(&instance.vm_id);
        Some(instance)
    }

    /// Run `visit` on the workload's instance.
    pub fn with<T>(&self, id: WorkloadId, visit: impl FnOnce(&mut Instance) -> T) -> Option<T> {
        lock(&self.inner).instances.get_mut(&id).map(visit)
    }

    /// Run `visit` on the instance a plugin callback names by its VM identifier.
    pub fn with_vm<T>(&self, vm_id: &str, visit: impl FnOnce(&mut Instance) -> T) -> Option<T> {
        let mut inner = lock(&self.inner);
        let id = *inner.by_vm.get(vm_id)?;
        inner.instances.get_mut(&id).map(visit)
    }

    /// The instance whose machine identifier prefixes a runtime key
    /// (`{machine}::{key}`), and the rest of the key.
    pub fn by_runtime_key(&self, key: &str) -> Option<(Arc<WorkloadCredential>, String, String)> {
        let inner = lock(&self.inner);
        inner.instances.values().find_map(|instance| {
            key.strip_prefix(&format!("{}::", instance.machine_id))
                .map(|rest| {
                    (
                        instance.credential.clone(),
                        instance.machine_id.clone(),
                        rest.to_owned(),
                    )
                })
        })
    }

    pub fn observations(&self) -> Vec<(WorkloadId, Observation)> {
        lock(&self.inner)
            .instances
            .iter()
            .map(|(id, instance)| (*id, instance.observation.clone()))
            .collect()
    }

    /// Let a plugin address the machine's workload by another identifier (a runtime
    /// transaction key, a container name).
    pub fn alias_vm(&self, alias: &str, machine_id: &str) {
        let mut inner = lock(&self.inner);
        let id = inner
            .instances
            .values()
            .find(|instance| instance.machine_id == machine_id || instance.vm_id == alias)
            .map(|instance| instance.record.id);
        if let Some(id) = id {
            inner.by_vm.insert(alias.to_owned(), id);
        }
    }

    /// Forget an alias; an instance's own VM identifier stays.
    pub fn unalias_vm(&self, alias: &str) {
        let mut inner = lock(&self.inner);
        let primary = inner
            .by_vm
            .get(alias)
            .and_then(|id| inner.instances.get(id))
            .is_some_and(|instance| instance.vm_id == alias);
        if !primary {
            inner.by_vm.remove(alias);
        }
    }

    /// The first workload whose instance matches.
    pub fn find(&self, matches: impl Fn(&Instance) -> bool) -> Option<WorkloadId> {
        lock(&self.inner)
            .instances
            .iter()
            .find(|(_, instance)| matches(instance))
            .map(|(id, _)| *id)
    }

    /// The identity of the workload a container name was registered for.
    pub(crate) fn container_identity(
        &self,
        name: &str,
    ) -> Option<crate::docker_host::ContainerIdentity> {
        let inner = lock(&self.inner);
        let id = inner.by_vm.get(name)?;
        let instance = inner.instances.get(id)?;
        Some(crate::docker_host::ContainerIdentity {
            vm_id: instance.vm_id.clone(),
            creature_id: instance.machine_id.clone(),
            program_id: instance.machine_id.clone(),
            machine_id: instance.machine_id.clone(),
            entity_id: instance.entity_id.clone(),
        })
    }

    pub fn contains(&self, id: WorkloadId) -> bool {
        lock(&self.inner).instances.contains_key(&id)
    }

    pub fn running_state(&self, id: WorkloadId) -> Option<ObservedWorkloadState> {
        lock(&self.inner)
            .instances
            .get(&id)
            .map(|instance| instance.observation.state)
    }
}

/// Plugin-local state (the legacy link keys plugins use for their own bookkeeping),
/// persisted in the backend's state directory. It never holds guest data.
pub struct PluginState {
    path: PathBuf,
    values: Mutex<BTreeMap<String, String>>,
}

impl PluginState {
    /// Load from `path`, or start empty.
    #[must_use]
    pub fn open(path: PathBuf) -> Self {
        let values = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            values: Mutex::new(values),
        }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> String {
        lock(&self.values).get(key).cloned().unwrap_or_default()
    }

    #[must_use]
    pub fn by_prefix(&self, prefix: &str) -> Vec<String> {
        lock(&self.values)
            .range(prefix.to_owned()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(_, value)| value.clone())
            .collect()
    }

    /// Apply puts (`Some`) and deletes (`None`) and persist atomically (write, then
    /// rename).
    ///
    /// # Errors
    ///
    /// When the state file cannot be written.
    pub fn apply(&self, ops: &[(String, Option<String>)]) -> Result<(), String> {
        let mut values = lock(&self.values);
        for (key, value) in ops {
            match value {
                Some(value) => {
                    values.insert(key.clone(), value.clone());
                }
                None => {
                    values.remove(key);
                }
            }
        }
        let bytes = serde_json::to_vec(&*values).map_err(|error| error.to_string())?;
        let temporary = self.path.with_extension("tmp");
        std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
        std::fs::rename(&temporary, &self.path).map_err(|error| error.to_string())
    }
}
