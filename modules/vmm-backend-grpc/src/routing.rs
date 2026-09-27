//! Runtime-key routing across independently installed A504 backend processes.
//!
//! The default backend remains the rollback/recovery path. A configured runtime is
//! sent to its own digest-pinned backend process, so installing or replacing that
//! runtime does not rebuild `aseman-vmm` or the default backend.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use aseman_contracts::vmm::BuildRequest;
use aseman_domain::WorkloadId;
use aseman_domain::vmm::{
    Endpoint, LogRecord, Observation, OperationRecord, ReconcileAction, Usage, WorkloadRecord,
};
use aseman_ports::vmm::{BackendDescription, VmmBackend};
use aseman_ports::{PortError, PortResult};

pub struct RoutingBackend {
    default: Arc<dyn VmmBackend>,
    runtimes: BTreeMap<String, Arc<dyn VmmBackend>>,
}

impl RoutingBackend {
    #[must_use]
    pub fn new(
        default: Arc<dyn VmmBackend>,
        runtimes: BTreeMap<String, Arc<dyn VmmBackend>>,
    ) -> Self {
        Self { default, runtimes }
    }

    fn for_runtime(&self, runtime: &str) -> Arc<dyn VmmBackend> {
        self.runtimes
            .get(runtime)
            .cloned()
            .unwrap_or_else(|| self.default.clone())
    }

    fn for_workload(&self, workload: &WorkloadRecord) -> Arc<dyn VmmBackend> {
        self.for_runtime(&workload.spec.runtime)
    }

    fn all(&self) -> Vec<Arc<dyn VmmBackend>> {
        let mut seen = BTreeSet::<usize>::new();
        std::iter::once(self.default.clone())
            .chain(self.runtimes.values().cloned())
            .filter(|backend| {
                let pointer = Arc::as_ptr(backend) as *const () as usize;
                seen.insert(pointer)
            })
            .collect()
    }
}

impl VmmBackend for RoutingBackend {
    fn describe(&self) -> PortResult<BackendDescription> {
        let mut capabilities = BTreeMap::new();
        for backend in self.all() {
            for runtime in backend.describe()?.runtimes {
                capabilities
                    .entry(runtime.runtime.clone())
                    .or_insert(runtime);
            }
        }
        // A route is authoritative for its key. Refuse a configuration whose target
        // does not actually advertise that runtime instead of silently falling back.
        for (runtime, backend) in &self.runtimes {
            let description = backend.describe()?;
            let selected = description
                .runtimes
                .into_iter()
                .find(|candidate| &candidate.runtime == runtime)
                .ok_or(PortError::Unsupported("configured runtime backend"))?;
            capabilities.insert(runtime.clone(), selected);
        }
        Ok(BackendDescription {
            name: "runtime-router".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            contract: "1".to_owned(),
            runtimes: capabilities.into_values().collect(),
        })
    }

    fn step(&self, workload: &WorkloadRecord, action: ReconcileAction) -> PortResult<Observation> {
        self.for_workload(workload).step(workload, action)
    }

    fn observe_all(&self) -> PortResult<Vec<(WorkloadId, Observation)>> {
        let mut observations = BTreeMap::new();
        for backend in self.all() {
            for (id, observation) in backend.observe_all()? {
                if observations.insert(id, observation).is_some() {
                    return Err(PortError::Conflict);
                }
            }
        }
        Ok(observations.into_iter().collect())
    }

    fn run(
        &self,
        workload: Option<&WorkloadRecord>,
        operation: &OperationRecord,
    ) -> PortResult<String> {
        if let Some(workload) = workload {
            return self.for_workload(workload).run(Some(workload), operation);
        }
        let request: BuildRequest = serde_json::from_str(
            operation
                .request
                .as_deref()
                .ok_or_else(|| PortError::Failed("build request is absent".to_owned()))?,
        )
        .map_err(|error| PortError::Failed(format!("invalid build request: {error}")))?;
        self.for_runtime(&request.runtime).run(None, operation)
    }

    fn forward_http(&self, workload: &WorkloadRecord, request: &str) -> PortResult<String> {
        self.for_workload(workload).forward_http(workload, request)
    }

    fn put_file(&self, workload: &WorkloadRecord, path: &str, bytes: &[u8]) -> PortResult<()> {
        self.for_workload(workload).put_file(workload, path, bytes)
    }

    fn get_file(&self, workload: &WorkloadRecord, path: &str) -> PortResult<Vec<u8>> {
        self.for_workload(workload).get_file(workload, path)
    }

    fn endpoints(&self, workload: &WorkloadRecord) -> PortResult<Vec<Endpoint>> {
        self.for_workload(workload).endpoints(workload)
    }

    fn usage(&self, workload: &WorkloadRecord) -> PortResult<Usage> {
        self.for_workload(workload).usage(workload)
    }

    fn logs(
        &self,
        workload: &WorkloadRecord,
        after: u64,
        limit: usize,
    ) -> PortResult<Vec<LogRecord>> {
        self.for_workload(workload).logs(workload, after, limit)
    }

    fn verify(&self, runtime: &str, request: &str) -> PortResult<String> {
        self.for_runtime(runtime).verify(runtime, request)
    }
}
