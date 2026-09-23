//! [`VmmBackend`] over Nomad (P6-01).
//!
//! The backend owns no Aseman state. It turns an A504 request into a Nomad job
//! registration or a client-API read, and turns Nomad's allocation state back into an
//! Aseman observation. What Nomad does not do — pause, snapshots, execution proofs —
//! is refused, never approximated.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use aseman_contracts::vmm::{HttpRequest, HttpResponse};
use aseman_domain::vmm::{
    DeployConventions, Endpoint, LogRecord, LogStream, Observation, OperationKind, OperationRecord,
    PortProtocol, ReconcileAction, RuntimeCapabilities, Usage, WorkloadRecord,
};
use aseman_domain::{Generation, ObservedWorkloadState, WorkloadId};
use aseman_ports::vmm::{BackendDescription, VmmBackend};
use aseman_ports::{PortError, PortResult};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

use crate::client::{Allocation, Nomad};
use crate::job::{self, Execution, NetworkMode, TASK};

fn failed(error: impl std::fmt::Display) -> PortError {
    PortError::Failed(error.to_string())
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// A runtime this backend offers and how it is executed.
pub struct Runtime {
    pub capabilities: RuntimeCapabilities,
    pub execution: Execution,
}

/// What the backend has read from a workload's log streams so far, so that log
/// sequences increase across calls and `after` can be honored.
#[derive(Default)]
struct LogCursor {
    /// Byte offset already consumed per stream.
    offsets: BTreeMap<String, u64>,
    records: Vec<LogRecord>,
    next: u64,
}

pub struct NomadBackend {
    nomad: Nomad,
    runtimes: Vec<Runtime>,
    datacenters: Vec<String>,
    network: NetworkMode,
    /// Observation sequences are backend-wide and increasing.
    sequence: Mutex<u64>,
    logs: Mutex<BTreeMap<WorkloadId, LogCursor>>,
}

impl NomadBackend {
    /// Serve `runtimes` on `nomad`, placing in `datacenters`.
    ///
    /// # Errors
    ///
    /// When the cluster does not answer: a backend that cannot reach its scheduler
    /// must fail at startup, not on the first workload.
    pub fn start(
        nomad: Nomad,
        runtimes: Vec<Runtime>,
        datacenters: Vec<String>,
        network: NetworkMode,
    ) -> PortResult<Self> {
        nomad.agent_version()?;
        Ok(Self {
            nomad,
            runtimes,
            datacenters,
            network,
            sequence: Mutex::new(0),
            logs: Mutex::new(BTreeMap::new()),
        })
    }

    fn next_sequence(&self) -> u64 {
        let mut sequence = self
            .sequence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *sequence += 1;
        *sequence
    }

    fn runtime(&self, key: &str) -> PortResult<&Runtime> {
        self.runtimes
            .iter()
            .find(|runtime| runtime.capabilities.runtime == key)
            .ok_or(PortError::Unsupported("runtime"))
    }

    /// The workload's newest allocation, if it has one.
    fn allocation(&self, record: &WorkloadRecord) -> PortResult<Option<Allocation>> {
        Ok(self
            .nomad
            .allocations(&job::job_id(record.id))?
            .into_iter()
            .next())
    }

    /// The allocation that must exist for a data-plane call.
    fn running_allocation(&self, record: &WorkloadRecord) -> PortResult<Allocation> {
        self.allocation(record)?
            .filter(|allocation| allocation.client_status == "running")
            .ok_or(PortError::Conflict)
    }

    fn observation(
        &self,
        state: ObservedWorkloadState,
        generation: Generation,
        reason: Option<String>,
    ) -> Observation {
        Observation {
            state,
            generation,
            sequence: self.next_sequence(),
            reason,
            observed_at_millis: now_millis(),
        }
    }

    /// Whether Nomad has the job and it wants no allocation. A job Nomad does not
    /// have wants none either, so a purged workload reads as stopped, not pending.
    fn job_is_stopped(&self, id: &str) -> PortResult<bool> {
        Ok(self
            .nomad
            .job(id)?
            .is_none_or(|job| job.stop || job.status == "dead"))
    }

    /// What Nomad says about a job whose meta names `generation`.
    fn observe_job(
        &self,
        id: WorkloadId,
        generation: Generation,
        stopped: bool,
    ) -> PortResult<Observation> {
        let allocations = self.nomad.allocations(&job::job_id(id))?;
        let Some(allocation) = allocations.first() else {
            // No allocation yet: the job is placing while it wants one, and is
            // simply not running when it does not.
            let state = if stopped {
                ObservedWorkloadState::Stopped
            } else {
                ObservedWorkloadState::Pending
            };
            return Ok(self.observation(state, generation, None));
        };
        // A stopped job's last allocation lingers as `complete`; report the job.
        let state = if stopped && allocation.client_status != "running" {
            ObservedWorkloadState::Stopped
        } else {
            job::observed_state(&allocation.client_status)
        };
        let reason = Some(allocation.client_description.clone()).filter(|text| !text.is_empty());
        Ok(self.observation(state, generation, reason))
    }

    /// The address and ports of a running allocation.
    fn addresses(&self, allocation: &Allocation) -> PortResult<Vec<(String, String, u16)>> {
        let allocation = self
            .nomad
            .allocation(&allocation.id)?
            .ok_or(PortError::NotFound)?;
        let networks = allocation
            .allocated_resources
            .as_ref()
            .and_then(|resources| resources["Shared"]["Networks"].as_array().cloned())
            .or_else(|| {
                allocation
                    .resources
                    .as_ref()
                    .and_then(|resources| resources["Networks"].as_array().cloned())
            })
            .unwrap_or_default();
        let mut found = Vec::new();
        for network in networks {
            let host = network["IP"].as_str().unwrap_or_default().to_owned();
            for port in network["DynamicPorts"]
                .as_array()
                .into_iter()
                .chain(network["ReservedPorts"].as_array())
                .flatten()
            {
                let label = port["Label"].as_str().unwrap_or_default().to_owned();
                let value = port["Value"].as_u64().unwrap_or_default();
                if let Ok(value) = u16::try_from(value) {
                    found.push((label, host.clone(), value));
                }
            }
        }
        Ok(found)
    }

    /// Read both streams of the workload's log from where this backend stopped, and
    /// append what is new. Nomad serves bytes, so the backend turns them into the
    /// ordered records A504 promises.
    fn refresh_logs(&self, record: &WorkloadRecord) -> PortResult<()> {
        let Some(allocation) = self.allocation(record)? else {
            return Ok(());
        };
        let mut cursors = self
            .logs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cursor = cursors.entry(record.id).or_default();
        for (stream, kind) in [("stdout", LogStream::Stdout), ("stderr", LogStream::Stderr)] {
            let offset = cursor.offsets.get(stream).copied().unwrap_or(0);
            let text = self.nomad.logs(&allocation.id, TASK, stream, offset)?;
            if text.is_empty() {
                continue;
            }
            cursor
                .offsets
                .insert(stream.to_owned(), offset + text.len() as u64);
            let at_millis = now_millis();
            for line in text.lines() {
                cursor.next += 1;
                cursor.records.push(LogRecord {
                    sequence: cursor.next,
                    at_millis,
                    stream: kind,
                    line: line.chars().take(65_536).collect(),
                });
            }
        }
        Ok(())
    }
}

impl VmmBackend for NomadBackend {
    fn describe(&self) -> PortResult<BackendDescription> {
        Ok(BackendDescription {
            name: "nomad".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            contract: "1".to_owned(),
            runtimes: self
                .runtimes
                .iter()
                .map(|runtime| runtime.capabilities.clone())
                .collect(),
        })
    }

    fn step(&self, workload: &WorkloadRecord, action: ReconcileAction) -> PortResult<Observation> {
        let generation = workload.desired.generation;
        let id = job::job_id(workload.id);
        match action {
            ReconcileAction::None => {
                let stopped = self.job_is_stopped(&id)?;
                self.observe_job(workload.id, generation, stopped)
            }
            // A restart is a start of the current generation: Nomad reschedules the
            // allocation itself once the job asks for one again.
            ReconcileAction::Start | ReconcileAction::Stop | ReconcileAction::Restart => {
                let runtime = self.runtime(&workload.spec.runtime)?;
                let job = job::job(
                    workload,
                    &runtime.execution,
                    self.nomad.namespace(),
                    &self.datacenters,
                    &self.network,
                )?;
                self.nomad.register(&job)?;
                let stopped = action == ReconcileAction::Stop;
                let observation = self.observe_job(workload.id, generation, stopped)?;
                // A stop is reported as stopped as soon as Nomad has the desired
                // count: the allocation's own shutdown is the scheduler's to finish.
                Ok(if stopped {
                    self.observation(ObservedWorkloadState::Stopped, generation, None)
                } else {
                    observation
                })
            }
            ReconcileAction::Delete => {
                self.nomad.stop(&id, true)?;
                self.logs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&workload.id);
                Ok(self.observation(ObservedWorkloadState::Stopped, generation, None))
            }
            // Pause and resume are the worker agent's, per runtime (ADR 0010); the
            // scheduler has no allocation-level pause to call.
            ReconcileAction::Pause | ReconcileAction::Resume => {
                Err(PortError::Unsupported("pause on the Nomad backend"))
            }
            // Adoption is the VMM's decision, not a step the backend takes: the
            // workload is observed as it is and the operator is asked (ADR 0022).
            ReconcileAction::Adopt => {
                let stopped = self.job_is_stopped(&id)?;
                self.observe_job(workload.id, generation, stopped)
            }
        }
    }

    fn observe_all(&self) -> PortResult<Vec<(WorkloadId, Observation)>> {
        let mut observations = Vec::new();
        for summary in self.nomad.jobs("aseman-")? {
            let Some(id) = job::workload_of(&summary.id) else {
                continue;
            };
            // The generation is the job's, not the caller's: an allocation of an
            // older spec is never reported at the current generation.
            let generation = summary
                .meta
                .get("aseman.generation")
                .and_then(|text| text.parse::<u64>().ok())
                .and_then(|value| Generation::from_stored(value).ok())
                .unwrap_or(Generation::INITIAL);
            let stopped = summary.stop || summary.status == "dead";
            observations.push((id, self.observe_job(id, generation, stopped)?));
        }
        Ok(observations)
    }

    fn run(
        &self,
        workload: Option<&WorkloadRecord>,
        operation: &OperationRecord,
    ) -> PortResult<String> {
        let workload = workload.ok_or(PortError::NotFound)?;
        let runtime = self.runtime(&workload.spec.runtime)?;
        match operation.kind {
            // An invocation reaches the workload the only way the mapping gives:
            // its declared HTTP ingress. A runtime with none cannot be invoked.
            OperationKind::Invoke if runtime.capabilities.invocation => {
                let request = HttpRequest {
                    method: "POST".to_owned(),
                    path: "/".to_owned(),
                    query: None,
                    headers: BTreeMap::from([(
                        "content-type".to_owned(),
                        "application/json".to_owned(),
                    )]),
                    body: operation
                        .request
                        .as_ref()
                        .map(|text| URL_SAFE_NO_PAD.encode(text.as_bytes())),
                    port: None,
                };
                self.forward_http(workload, &serde_json::to_string(&request).map_err(failed)?)
            }
            OperationKind::Invoke => Err(PortError::Unsupported("invocation")),
            OperationKind::Build => {
                Err(PortError::Unsupported("building an image on the scheduler"))
            }
            OperationKind::Snapshot | OperationKind::Restore => {
                Err(PortError::Unsupported("snapshots on the Nomad backend"))
            }
            OperationKind::Exec => Err(PortError::Unsupported("exec on the Nomad backend")),
            _ => Err(PortError::Unsupported("operation")),
        }
    }

    fn forward_http(&self, workload: &WorkloadRecord, request: &str) -> PortResult<String> {
        let request: HttpRequest = serde_json::from_str(request).map_err(failed)?;
        let allocation = self.running_allocation(workload)?;
        let addresses = self.addresses(&allocation)?;
        let wanted = workload
            .spec
            .network
            .ingress
            .iter()
            .find(|port| match &request.port {
                Some(label) => port.name == *label,
                None => port.protocol == PortProtocol::Http,
            })
            .ok_or(PortError::Unsupported("HTTP ingress"))?;
        let (_, host, port) = addresses
            .into_iter()
            .find(|(label, _, _)| *label == wanted.name)
            .ok_or(PortError::Conflict)?;
        let mut url = format!("http://{host}:{port}{}", request.path);
        if let Some(query) = &request.query {
            url.push('?');
            url.push_str(query);
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(failed)?;
        let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(failed)?;
        let mut builder = client.request(method, url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.body(URL_SAFE_NO_PAD.decode(body).map_err(failed)?);
        }
        let response = builder
            .send()
            .map_err(|_| PortError::Unavailable("the workload"))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
            })
            .collect();
        let bytes = response.bytes().map_err(failed)?;
        serde_json::to_string(&HttpResponse {
            status,
            headers,
            body: (!bytes.is_empty()).then(|| URL_SAFE_NO_PAD.encode(&bytes)),
        })
        .map_err(failed)
    }

    fn put_file(&self, _workload: &WorkloadRecord, _path: &str, _bytes: &[u8]) -> PortResult<()> {
        // Nomad's client API reads an allocation directory; it does not write one.
        // Writing into a running workload is the worker agent's (P6-05).
        Err(PortError::Unsupported("writing files on the Nomad backend"))
    }

    fn get_file(&self, workload: &WorkloadRecord, path: &str) -> PortResult<Vec<u8>> {
        let runtime = self.runtime(&workload.spec.runtime)?;
        if !runtime.capabilities.files {
            return Err(PortError::Unsupported("files"));
        }
        let allocation = self.running_allocation(workload)?;
        self.nomad
            .read_file(&allocation.id, &format!("{TASK}/{path}"))
    }

    fn endpoints(&self, workload: &WorkloadRecord) -> PortResult<Vec<Endpoint>> {
        let Some(allocation) = self.allocation(workload)? else {
            return Ok(Vec::new());
        };
        if allocation.client_status != "running" {
            return Ok(Vec::new());
        }
        let declared: BTreeMap<_, _> = workload
            .spec
            .network
            .ingress
            .iter()
            .map(|port| (port.name.clone(), port.protocol))
            .collect();
        Ok(self
            .addresses(&allocation)?
            .into_iter()
            .filter_map(|(label, host, port)| {
                declared.get(&label).map(|protocol| Endpoint {
                    name: label.clone(),
                    address: host,
                    port,
                    protocol: *protocol,
                })
            })
            .collect())
    }

    fn usage(&self, workload: &WorkloadRecord) -> PortResult<Usage> {
        let sequence = self.next_sequence();
        let now = now_millis();
        let Some(allocation) = self.allocation(workload)? else {
            return Ok(Usage {
                sequence,
                window_start_millis: now,
                window_end_millis: now,
                ..Usage::default()
            });
        };
        // A client that cannot be reached is not a failed usage read: the workload
        // may be between allocations. Report what is known with a fresh sequence.
        let stats = self.nomad.stats(&allocation.id).unwrap_or_default();
        let stats = stats.unwrap_or_default();
        let usage = &stats["ResourceUsage"];
        Ok(Usage {
            sequence,
            window_start_millis: now,
            window_end_millis: now,
            cpu_millis: usage["CpuStats"]["TotalTicks"]
                .as_f64()
                .filter(|ticks| ticks.is_finite() && *ticks >= 0.0)
                .map_or(0, |ticks| ticks as u64),
            memory_peak_bytes: usage["MemoryStats"]["MaxUsage"]
                .as_u64()
                .or_else(|| usage["MemoryStats"]["RSS"].as_u64())
                .unwrap_or_default(),
            network_rx_bytes: 0,
            network_tx_bytes: 0,
            storage_bytes: 0,
            invocations: 0,
        })
    }

    fn logs(
        &self,
        workload: &WorkloadRecord,
        after: u64,
        limit: usize,
    ) -> PortResult<Vec<LogRecord>> {
        self.refresh_logs(workload)?;
        Ok(self
            .logs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&workload.id)
            .map(|cursor| {
                cursor
                    .records
                    .iter()
                    .filter(|record| record.sequence > after)
                    .take(limit)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    fn verify(&self, _runtime: &str, _request: &str) -> PortResult<String> {
        Err(PortError::Unsupported("execution proofs"))
    }
}

/// The A505 runtime matrix: what each runtime can do, whatever runs it.
const PARITY_JSON: &str = include_str!("../../../../docs/generated/vmm-native-parity.json");

/// What this backend can deliver for `runtime`, however it is executed.
///
/// The capabilities are not asserted here: they are the runtime's own, from the
/// generated A505 matrix, with everything Nomad cannot deliver masked off. A backend
/// that claimed more would be lying to the scheduler and to the caller.
///
/// # Errors
///
/// [`PortError::Unsupported`] when the matrix does not describe the runtime.
pub fn runtime_capabilities(
    runtime: &str,
    execution: &Execution,
) -> PortResult<RuntimeCapabilities> {
    let parity: Value = serde_json::from_str(PARITY_JSON).map_err(failed)?;
    let flags = parity["runtimes"]
        .get(runtime)
        .ok_or(PortError::Unsupported("runtime"))?;
    let flag = |name: &str| flags[name].as_bool().unwrap_or(false);
    let deploy: DeployConventions =
        serde_json::from_value(flags["deploy"].clone()).map_err(failed)?;
    // A runner speaks to the node over the guest API; it serves no HTTP of its own
    // unless the runtime does.
    let http = flag("http_ingress");
    Ok(RuntimeCapabilities {
        runtime: runtime.to_owned(),
        // An invocation reaches a workload through its HTTP ingress, which is the
        // only way into an allocation the mapping gives.
        invocation: flag("invocation") && http,
        long_running: flag("long_running"),
        // Nomad has no allocation-level pause or snapshot; both belong to the worker
        // agent (ADR 0010, P6-05).
        pause: false,
        snapshot: false,
        // Nomad's exec is a websocket the backend does not speak yet (P6-05).
        exec: false,
        terminal: false,
        http_ingress: http,
        // Nomad's client API reads an allocation directory; it does not write one, so
        // a runtime's file support is read-only here.
        files: flag("files"),
        // An image is built before registration; the scheduler does not build.
        build: false,
        chain_transactions: flag("chain_transactions"),
        // Proofs are the runtime's, and the runner would have to carry them out.
        execution_proofs: flag("execution_proofs") && matches!(execution, Execution::Runner(_)),
        deploy,
    })
}
