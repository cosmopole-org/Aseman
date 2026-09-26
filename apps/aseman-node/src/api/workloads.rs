//! Remote workloads (P5-03, P5-04, ADR 0029, ADR 0030).
//!
//! When the node is configured with a VMM (`ASEMAN_VMM_ENDPOINT`), program entities
//! run as workloads of that VMM instead of the embedded one:
//! - each VM instance is a `core.workload` capsule with a deterministic ID, recorded
//!   before the VMM is asked for it;
//! - each workload gets its own Ed25519 key, registered in the key directory; the
//!   private half reaches the VMM only as the write-only bootstrap credential;
//! - lifecycle changes go through `SetDesiredWorkloadState` (desired state first,
//!   generation-keyed commands);
//! - signals become invocations of the entity's `signal` workload;
//! - the workload's host calls come back through the node's guest API, which
//!   authenticates the workload's proof, resolves its creature and program from the
//!   trusted records, and serves the call on the node's ordinary, authorized host
//!   call path.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufReader;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Result, anyhow};
use aseman_application::SetDesiredWorkloadState;
use aseman_application::guest_call::{
    GuestRequest, ProvisionWorkload, ServeGuestCall, WorkloadKey,
};
use aseman_application::identity::{IdentityFailure, VerifierPolicy};
use aseman_capsule::capability::CapsuleGrantStore;
use aseman_capsule::entity::CapsuleEntityPorts;
use aseman_capsule::identity::CapsuleKeyDirectory;
use aseman_capsule::workload::CapsuleWorkloads;
use aseman_config::VmmClientConfig;
use aseman_contracts::guest_api::{ARTIFACT_ACTION, WorkloadCredential, audience, call_action};
use aseman_contracts::legacy_realtime::deterministic_legacy_capsule_id;
use aseman_contracts::vmm::{Invocation, InvocationKind};
use aseman_domain::authority::Condition;
use aseman_domain::identity::{FreshnessPolicy, Proof, RotationPolicy, Subject, SubjectKind};
use aseman_domain::program::ArtifactRole;
use aseman_domain::vmm::{
    Artifact, ArtifactKind, Bootstrap, NetworkPolicy, Resources, WorkloadLabels, WorkloadSpec,
    WriteOnlyCredential,
};
use aseman_domain::{
    CreatureId, DesiredWorkload, DesiredWorkloadState, Generation, ProgramId, Uuid, WorkloadId,
};
use aseman_guest_http::server::GuestApi;
use aseman_identity_native::NativeIdentityVerifier;
use aseman_ports::guest::{GuestCaller, GuestHostCalls};
use aseman_ports::{
    BlobStore, ClockPort, EntityDirectory, PortError, PortResult, WorkloadRepository,
};
use aseman_storage_postgres::PostgresCapsuleRepository;
use aseman_vmm_http::client::{ClientTls, HttpVmmClient};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::drivers::blob_store::StorageRootBlobStore;

/// The VM instance that serves an entity's signals.
pub(crate) const SIGNAL_INSTANCE: &str = "signal";

struct SystemClock;

impl ClockPort for SystemClock {
    fn unix_millis(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

/// What a launched instance may use (the legacy `resources` input, normalized).
#[derive(Clone, Copy, Debug)]
pub(crate) struct LaunchResources {
    pub(crate) cpu_cores: i64,
    pub(crate) ram_mb: i64,
    pub(crate) disk_gb: i64,
    pub(crate) max_exec_time_seconds: i64,
}

impl Default for LaunchResources {
    fn default() -> Self {
        Self {
            cpu_cores: 1,
            ram_mb: 64,
            disk_gb: 1,
            max_exec_time_seconds: 60,
        }
    }
}

pub(crate) struct RemoteWorkloads {
    client: HttpVmmClient,
    /// The VMM's runtimes, read once (capabilities change only with a new backend).
    runtimes: std::sync::Mutex<Option<Vec<aseman_domain::vmm::RuntimeCapabilities>>>,
    catalog: PostgresCapsuleRepository,
    blobs: StorageRootBlobStore,
    guest_api_url: String,
    audience: String,
}

static REMOTE: OnceLock<Arc<RemoteWorkloads>> = OnceLock::new();

/// The remote VMM, when the node is configured with one.
pub(crate) fn remote() -> Option<Arc<RemoteWorkloads>> {
    REMOTE.get().cloned()
}

fn unsigned(value: i64) -> u64 {
    u64::try_from(value.max(0)).unwrap_or(0)
}

fn capsule(family: &str, legacy_id: &str) -> Uuid {
    Uuid::from_bytes(deterministic_legacy_capsule_id(
        family,
        legacy_id.as_bytes(),
    ))
}

/// A legacy creature's typed identity.
pub(crate) fn creature_subject(legacy_id: &str) -> Subject {
    Subject {
        kind: SubjectKind::Creature,
        id: capsule("Creature", legacy_id),
    }
}

/// The node's typed identity (ADR 0009): its legacy ID's deterministic capsule ID.
pub(crate) fn node_subject(node_id: &str) -> Subject {
    Subject {
        kind: SubjectKind::Node,
        id: capsule("Node", node_id),
    }
}

impl RemoteWorkloads {
    /// The workload of one VM instance of a program entity.
    pub(crate) fn workload_id(program: &str, entity: &str, vm: &str) -> WorkloadId {
        WorkloadId::from_uuid(capsule("Workload", &[program, entity, vm].join("\0")))
    }

    fn workloads(&self) -> CapsuleWorkloads<'_> {
        CapsuleWorkloads {
            repository: &self.catalog,
        }
    }

    /// Record, key, and create one VM instance of `entity`; safe to repeat.
    pub(crate) fn launch(
        &self,
        program: &str,
        machine: &str,
        entity: &str,
        vm: &str,
        runtime: &str,
        resources: LaunchResources,
        environment: BTreeMap<String, String>,
    ) -> Result<WorkloadId> {
        let id = Self::workload_id(program, entity, vm);
        let store_key = CapsuleEntityPorts {
            repository: &self.catalog,
        }
        .artifact(program, entity, ArtifactRole::Primary)
        .map_err(|error| anyhow!("{error}"))?
        .and_then(|artifact| artifact.store_key)
        .ok_or_else(|| anyhow!("the entity has no deployed file"))?;
        let bytes = self
            .blobs
            .blob(&store_key)
            .map_err(|error| anyhow!("{error}"))?
            .ok_or_else(|| anyhow!("the entity's file is missing"))?;
        let desired = DesiredWorkload {
            id,
            creature_id: CreatureId::from_uuid(capsule("Creature", machine)),
            program_id: ProgramId::from_uuid(capsule("Program", program)),
            name: format!("{entity}/{vm}"),
            runtime: runtime.to_owned(),
            generation: Generation::INITIAL,
            state: DesiredWorkloadState::Running,
        };
        let labels = WorkloadLabels {
            creature_id: *desired.creature_id.as_uuid(),
            program_id: *desired.program_id.as_uuid(),
            entity_id: entity.to_owned(),
            legacy_machine_id: Some(program.to_owned()),
            legacy_vm_id: Some(vm.to_owned()),
        };
        let spec = WorkloadSpec {
            runtime: runtime.to_owned(),
            artifact: Artifact {
                kind: ArtifactKind::Blob,
                reference: store_key,
                digest: format!("sha256:{}", hex::encode(Sha256::digest(&bytes))),
            },
            entry: entity.to_owned(),
            resources: Resources {
                vcpu_millis: unsigned(resources.cpu_cores).max(1) * 1000,
                memory_mib: unsigned(resources.ram_mb).max(1),
                disk_mib: Some(unsigned(resources.disk_gb) * 1024),
                invocation_timeout_millis: Some(unsigned(resources.max_exec_time_seconds) * 1000),
            },
            network: NetworkPolicy::default(),
            environment,
            bootstrap: Bootstrap {
                guest_api_url: self.guest_api_url.clone(),
                credential: None,
            },
        };
        let credential = WorkloadCredential::generate(id, &self.guest_api_url, &self.audience)
            .map_err(|error| anyhow!("{error}"))?;
        let encoded = credential.encode();
        let for_epoch = |epoch: u32| -> Result<WriteOnlyCredential, PortError> {
            WorkloadCredential::decode(&encoded)
                .and_then(|copy| copy.at_epoch(epoch))
                .map(|credential| WriteOnlyCredential::new(credential.encode()))
                .map_err(|_| PortError::Failed("unusable workload credential".to_owned()))
        };
        let workloads = self.workloads();
        ProvisionWorkload {
            workloads: &workloads,
            keys: &CapsuleKeyDirectory {
                repository: &self.catalog,
            },
            verifier: &NativeIdentityVerifier,
            clock: &SystemClock,
            vmm: &self.client,
        }
        .execute(
            &desired,
            labels,
            spec,
            &WorkloadKey {
                public_key: credential.public_key(),
                credential_for_epoch: &for_epoch,
            },
        )
        .map_err(|error| anyhow!("{error}"))?;
        Ok(id)
    }

    /// Change a workload's desired state as `user`, the owner of its program (the
    /// caller verified the ownership).
    pub(crate) fn set_state(
        &self,
        user: &str,
        workload: WorkloadId,
        state: DesiredWorkloadState,
    ) -> Result<u64> {
        self.set_state_as(
            Subject {
                kind: SubjectKind::User,
                id: capsule("Creature", user),
            },
            workload,
            state,
        )
    }

    /// Change a workload's desired state as `actor`, whose ownership of the workload
    /// the node's decision point already established.
    pub(crate) fn set_state_as(
        &self,
        actor: Subject,
        workload: WorkloadId,
        state: DesiredWorkloadState,
    ) -> Result<u64> {
        let policy = crate::shell::authority::policy()
            .ok_or_else(|| anyhow!("the action registry did not load"))?;
        let workloads = self.workloads();
        SetDesiredWorkloadState {
            workloads: &workloads,
            policy,
            grants: &CapsuleGrantStore {
                repository: &self.catalog,
            },
            vmm: &self.client,
            clock: &SystemClock,
        }
        .execute(actor, workload, state, &BTreeSet::from([Condition::Owner]))
        .map_err(|error| anyhow!("{error}"))
    }

    /// Whether the node records this workload.
    pub(crate) fn exists(&self, workload: WorkloadId) -> Result<bool> {
        Ok(self
            .workloads()
            .get_desired(workload)
            .map_err(|error| anyhow!("{error}"))?
            .is_some_and(|workload| workload.state != DesiredWorkloadState::Deleted))
    }

    /// Deliver one signal (the legacy `Send` packet) to the entity's signal workload,
    /// launching it on first use.
    pub(crate) fn invoke(
        &self,
        program: &str,
        machine: &str,
        entity: &str,
        runtime: &str,
        store_id: &str,
        packet: Value,
        chain: bool,
    ) -> Result<()> {
        let id = Self::workload_id(program, entity, SIGNAL_INSTANCE);
        if !self.exists(id)? {
            self.launch(
                program,
                machine,
                entity,
                SIGNAL_INSTANCE,
                runtime,
                LaunchResources::default(),
                BTreeMap::new(),
            )?;
        }
        let invocation = Invocation {
            kind: if chain {
                InvocationKind::ChainTransactions
            } else {
                InvocationKind::Signal
            },
            key: "creatures/signal".to_owned(),
            store_id: (!store_id.is_empty()).then(|| store_id.to_owned()),
            payload: packet,
        };
        let body = serde_json::to_string(&invocation)?;
        aseman_ports::vmm::VmmClient::invoke(
            &self.client,
            id,
            &body,
            &format!("invoke-{}", Uuid::now_v7().simple()),
        )
        .map_err(|error| anyhow!("{error}"))?;
        Ok(())
    }
}

impl RemoteWorkloads {
    /// Forward a legacy ingress request (`{ programId, entityId, vmId, method, path,
    /// query, headers, bodyBase64 }`) to its workload; the answer is the legacy
    /// `{ ok, status, headers, bodyBase64 }`. The entity's signal workload serves a
    /// request that names no instance.
    pub(crate) fn forward_http(&self, request: &Value) -> Result<Value> {
        use base64::Engine;
        let text = |field: &str| request[field].as_str().unwrap_or("").trim().to_owned();
        let program = text("programId");
        let entity = if text("entityId").is_empty() {
            aseman_domain::program::DEFAULT_ALARM_ENTITY.to_owned()
        } else {
            text("entityId")
        };
        let vm = if text("vmId").is_empty() {
            SIGNAL_INSTANCE.to_owned()
        } else {
            text("vmId")
        };
        let body = base64::engine::general_purpose::STANDARD
            .decode(text("bodyBase64"))
            .map_err(|_| anyhow!("the request body is not base64"))?;
        let headers: BTreeMap<String, String> = request["headers"]
            .as_object()
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        let wire = aseman_contracts::vmm::HttpRequest {
            method: if text("method").is_empty() {
                "GET".to_owned()
            } else {
                text("method").to_uppercase()
            },
            path: if text("path").is_empty() {
                "/".to_owned()
            } else {
                text("path")
            },
            query: (!text("query").is_empty()).then(|| text("query")),
            headers,
            body: Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(body)),
            port: None,
        };
        let answer = aseman_ports::vmm::VmmClient::forward_http(
            &self.client,
            Self::workload_id(&program, &entity, &vm),
            &serde_json::to_string(&wire)?,
            &format!("http-{}", Uuid::now_v7().simple()),
        )
        .map_err(|error| match error {
            PortError::Unsupported(_) => anyhow!("unsupported: the runtime has no ingress"),
            other => anyhow!("{other}"),
        })?;
        let response: aseman_contracts::vmm::HttpResponse = serde_json::from_str(&answer)?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(response.body.unwrap_or_default())
            .map_err(|_| anyhow!("the VMM answered an invalid body"))?;
        Ok(json!({
            "ok": true,
            "status": response.status,
            "headers": response.headers,
            "bodyBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
        }))
    }
}

/// How long a host call waits for an operation it started.
const HOST_CALL_WAIT: Duration = Duration::from_secs(120);

fn refused(reason: impl std::fmt::Display) -> String {
    json!({"ok": false, "error": reason.to_string()}).to_string()
}

impl RemoteWorkloads {
    /// The VMM's capabilities for `runtime`.
    fn runtime(&self, runtime: &str) -> Result<aseman_domain::vmm::RuntimeCapabilities> {
        let mut cached = self
            .runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cached.is_none() {
            *cached = Some(
                aseman_ports::vmm::VmmClient::capabilities(&self.client)
                    .map_err(|error| anyhow!("{error}"))?
                    .runtimes,
            );
        }
        cached
            .as_ref()
            .and_then(|runtimes| runtimes.iter().find(|entry| entry.runtime == runtime))
            .cloned()
            .ok_or_else(|| anyhow!("the VMM offers no runtime {runtime}"))
    }

    /// Whether the VMM offers `runtime`.
    pub(crate) fn offers(&self, runtime: &str) -> bool {
        self.runtime(runtime).is_ok()
    }

    /// The runtime keys the VMM offers.
    pub(crate) fn runtime_keys(&self) -> Vec<String> {
        let _ = self.runtime("");
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|runtimes| runtimes.iter().map(|entry| entry.runtime.clone()).collect())
            .unwrap_or_default()
    }

    /// A workload's log lines after `after`, oldest first (A501 `logs`). A workload's
    /// logs are its VMM's: the node stores none of its own.
    pub(crate) fn logs(
        &self,
        workload: WorkloadId,
        after: u64,
    ) -> Result<Vec<aseman_domain::vmm::LogRecord>> {
        aseman_ports::vmm::VmmClient::logs(&self.client, workload, after)
            .map_err(|error| anyhow!("{error}"))
    }

    /// The deploy conventions of `runtime`.
    pub(crate) fn deploy_conventions(
        &self,
        runtime: &str,
    ) -> Option<aseman_domain::vmm::DeployConventions> {
        self.runtime(runtime).ok().map(|entry| entry.deploy)
    }

    /// Wait for an operation to finish; its A501 result or failure.
    fn await_operation(
        &self,
        operation: aseman_domain::vmm::OperationRecord,
    ) -> Result<Option<String>> {
        use aseman_domain::OperationState;
        let deadline = std::time::Instant::now() + HOST_CALL_WAIT;
        let mut current = operation;
        loop {
            match current.state {
                OperationState::Succeeded => return Ok(current.result),
                OperationState::Failed | OperationState::Cancelled => {
                    return Err(anyhow!(
                        "{}",
                        current
                            .error
                            .map(|failure| format!("{}: {}", failure.code.as_str(), failure.detail))
                            .unwrap_or_else(|| "the operation did not complete".to_owned())
                    ));
                }
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                return Err(anyhow!("the operation is still running"));
            }
            std::thread::sleep(Duration::from_millis(50));
            current = aseman_ports::vmm::VmmClient::operation(&self.client, current.id)
                .map_err(|error| anyhow!("{error}"))?
                .ok_or_else(|| anyhow!("the operation disappeared"))?;
        }
    }

    fn key(prefix: &str) -> String {
        format!("{prefix}-{}", Uuid::now_v7().simple())
    }

    /// A VM host call (`runVm`, `terminateVm`, `deleteVm`, `execVm`, `statusVm`,
    /// `copyToVm`, `copyFromVm`, `buildVmImage`, `vmEndpoints`) for `caller` (the
    /// node-resolved calling program), already authorized by the node's decision
    /// point. The target is `{machineId, entityId, vmId}` of `input`; the answer keeps
    /// the legacy shapes.
    pub(crate) fn vm_host_call(
        &self,
        app: &Arc<dyn crate::models::core::ICore>,
        op: &str,
        caller: &str,
        input: &Value,
    ) -> String {
        match self.vm_host_call_inner(app, op, caller, input) {
            Ok(answer) => answer.to_string(),
            Err(error) => refused(error),
        }
    }

    fn vm_host_call_inner(
        &self,
        app: &Arc<dyn crate::models::core::ICore>,
        op: &str,
        caller: &str,
        input: &Value,
    ) -> Result<Value> {
        use base64::Engine;
        let text = |field: &str| input[field].as_str().unwrap_or("").trim().to_owned();
        let program = if text("machineId").is_empty() {
            caller.to_owned()
        } else {
            text("machineId")
        };
        let entity = if text("entityId").is_empty() {
            aseman_domain::program::DEFAULT_ALARM_ENTITY.to_owned()
        } else {
            text("entityId")
        };
        let runtime = [text("runtime"), text("vmType")]
            .into_iter()
            .find(|value| !value.is_empty())
            .map(|value| value.to_lowercase())
            .unwrap_or_else(|| crate::drivers::vmm::driver::entity_runtime(app, &program, &entity));
        let vm = text("vmId");
        let target = |vm: &str| Self::workload_id(&program, &entity, vm);
        let as_caller = Subject {
            kind: SubjectKind::Creature,
            id: capsule("Creature", caller),
        };
        let client = &self.client;
        Ok(match op {
            "runVm" => {
                let capabilities = self.runtime(&runtime)?;
                if !capabilities.long_running {
                    // An invocation runtime runs the program once and answers.
                    let machine = program_machine(app, &program);
                    let payload = input.get("input").cloned().unwrap_or(Value::Null);
                    let id = Self::workload_id(&program, &entity, SIGNAL_INSTANCE);
                    if !self.exists(id)? {
                        self.launch(
                            &program,
                            &machine,
                            &entity,
                            SIGNAL_INSTANCE,
                            &runtime,
                            LaunchResources::default(),
                            BTreeMap::new(),
                        )?;
                    }
                    let invocation = Invocation {
                        kind: InvocationKind::Signal,
                        key: "runVm".to_owned(),
                        store_id: None,
                        payload,
                    };
                    let operation = aseman_ports::vmm::VmmClient::invoke(
                        client,
                        id,
                        &serde_json::to_string(&invocation)?,
                        &Self::key("run"),
                    )
                    .map_err(|error| anyhow!("{error}"))?;
                    let result = self.await_operation(operation)?.unwrap_or_default();
                    let result: Value = serde_json::from_str(&result).unwrap_or_default();
                    let mut answer = result["output"].clone();
                    if !answer.is_object() {
                        answer = json!({"output": answer});
                    }
                    answer["ok"] = answer.get("ok").cloned().unwrap_or(json!(true));
                    answer
                } else {
                    let vm = if vm.is_empty() {
                        Uuid::now_v7().to_string()
                    } else {
                        vm
                    };
                    let machine = program_machine(app, &program);
                    let resources = &input["resources"];
                    let number =
                        |field: &str, default: i64| resources[field].as_i64().unwrap_or(default);
                    let environment = input["params"]
                        .as_object()
                        .map(|params| {
                            params
                                .iter()
                                .map(|(name, value)| {
                                    (
                                        name.clone(),
                                        value
                                            .as_str()
                                            .map_or_else(|| value.to_string(), str::to_owned),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    self.launch(
                        &program,
                        &machine,
                        &entity,
                        &vm,
                        &runtime,
                        LaunchResources {
                            cpu_cores: number("cpuCores", 1),
                            ram_mb: number("ramMb", 64),
                            disk_gb: number("diskGb", 1),
                            max_exec_time_seconds: number("maxExecTimeSeconds", 60),
                        },
                        environment,
                    )?;
                    json!({"ok": true, "vmId": vm, "machineId": program})
                }
            }
            "terminateVm" | "deleteVm" | "destroyVm" => {
                let state = if op == "terminateVm" {
                    DesiredWorkloadState::Stopped
                } else {
                    DesiredWorkloadState::Deleted
                };
                let generation = self.set_state_as(as_caller, target(&vm), state)?;
                json!({"ok": true, "vmId": vm, "generation": generation})
            }
            "execVm" | "execDocker" => {
                let mut command: Vec<String> = input["args"]
                    .as_array()
                    .map(|args| {
                        args.iter()
                            .filter_map(|arg| arg.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                if command.is_empty() {
                    command = vec!["sh".to_owned(), "-lc".to_owned(), text("command")];
                }
                let request = aseman_contracts::vmm::ExecRequest {
                    command,
                    stdin: None,
                    timeout_millis: input["timeoutSecs"].as_u64().map(|seconds| seconds * 1000),
                };
                let operation = aseman_ports::vmm::VmmClient::exec(
                    client,
                    target(&vm),
                    &serde_json::to_string(&request)?,
                    &Self::key("exec"),
                )
                .map_err(|error| anyhow!("{error}"))?;
                let result: aseman_contracts::vmm::ExecResult =
                    serde_json::from_str(&self.await_operation(operation)?.unwrap_or_default())?;
                let decode = |text: &str| {
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(text)
                        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                        .unwrap_or_default()
                };
                json!({
                    "ok": result.exit_code == 0,
                    "exitCode": result.exit_code,
                    "stdout": decode(&result.stdout),
                    "stderr": decode(&result.stderr),
                })
            }
            "statusVm" => {
                let workload = aseman_ports::vmm::VmmClient::workload(client, target(&vm))
                    .map_err(|error| anyhow!("{error}"))?
                    .ok_or_else(|| anyhow!("no such vm"))?;
                let observed = workload.observed.as_ref();
                json!({
                    "ok": true,
                    "vmId": vm,
                    "status": observed.map_or_else(|| "pending".to_owned(), |observed| serde_json::to_value(observed.state).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default()),
                    "running": observed.is_some_and(|observed| observed.state == aseman_domain::ObservedWorkloadState::Running),
                    "error": observed.and_then(|observed| observed.reason.clone()),
                })
            }
            "copyToVm" | "copyToDocker" => {
                let path = [text("targetPath"), text("fileName")]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("/");
                let path = if path.is_empty() { text("path") } else { path };
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(text("content"))
                    .unwrap_or_else(|_| text("content").into_bytes());
                aseman_ports::vmm::VmmClient::put_file(
                    client,
                    target(&vm),
                    path.trim_start_matches('/'),
                    &bytes,
                    &Self::key("put"),
                )
                .map_err(|error| anyhow!("{error}"))?;
                json!({"ok": true})
            }
            "copyFromVm" => {
                let bytes = aseman_ports::vmm::VmmClient::get_file(
                    client,
                    target(&vm),
                    text("path").trim_start_matches('/'),
                )
                .map_err(|error| anyhow!("{error}"))?;
                json!({"ok": true, "content": base64::engine::general_purpose::STANDARD.encode(bytes)})
            }
            "buildVmImage" | "buildDockerImage" => {
                let workload = aseman_ports::vmm::VmmClient::workload(client, target(&vm))
                    .map_err(|error| anyhow!("{error}"))?
                    .ok_or_else(|| anyhow!("build a deployed entity with a running workload"))?;
                let request = aseman_contracts::vmm::BuildRequest {
                    id: Uuid::now_v7(),
                    runtime: runtime.clone(),
                    source: workload.spec.artifact.clone(),
                    entry: Some(workload.spec.entry.clone()),
                    build_type: (!text("buildType").is_empty()).then(|| text("buildType")),
                    labels: workload.labels,
                };
                let operation = aseman_ports::vmm::VmmClient::build(
                    client,
                    &serde_json::to_string(&request)?,
                    &Self::key("build"),
                )
                .map_err(|error| anyhow!("{error}"))?;
                self.await_operation(operation)?;
                json!({"ok": true})
            }
            "vmEndpoints" => {
                let endpoints = aseman_ports::vmm::VmmClient::endpoints(client, target(&vm))
                    .map_err(|error| anyhow!("{error}"))?;
                json!({"ok": true, "endpoints": endpoints})
            }
            other => return Err(anyhow!("{other} is not a VM host call")),
        })
    }

    /// `verifyProgramExecution`: verify a proof of one of the node's program files
    /// (`masmPath` under the node's storage), by the VMM runtime that proves them.
    pub(crate) fn verify_execution(&self, input: &Value) -> String {
        match self.verify_inner(input) {
            Ok(answer) => answer,
            Err(error) => refused(error),
        }
    }

    fn verify_inner(&self, input: &Value) -> Result<String> {
        use base64::Engine;
        let path = input["masmPath"].as_str().unwrap_or("");
        let key = self
            .blobs
            .key_of(path)
            .ok_or_else(|| anyhow!("masmPath must name a program file of this node"))?;
        let bytes = self
            .blobs
            .blob(&key)
            .map_err(|error| anyhow!("{error}"))?
            .ok_or_else(|| anyhow!("the program file does not exist"))?;
        let numbers = |field: &str| -> Vec<u64> {
            input[field]
                .as_array()
                .map(|values| values.iter().filter_map(Value::as_u64).collect())
                .unwrap_or_default()
        };
        let proof: Vec<u8> = input["proof"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_u64().and_then(|byte| u8::try_from(byte).ok()))
                    .collect()
            })
            .unwrap_or_default();
        let runtime = self
            .runtime_keys()
            .into_iter()
            .find(|key| self.runtime(key).is_ok_and(|entry| entry.execution_proofs))
            .ok_or_else(|| anyhow!("no runtime of the VMM verifies program executions"))?;
        let request = aseman_contracts::vmm::VerificationRequest {
            program: Artifact {
                kind: ArtifactKind::Blob,
                reference: key,
                digest: format!("sha256:{}", hex::encode(Sha256::digest(&bytes))),
            },
            inputs: numbers("inputs"),
            outputs: numbers("outputs"),
            proof: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(proof),
        };
        let answer = aseman_ports::vmm::VmmClient::verify(
            &self.client,
            &runtime,
            &serde_json::to_string(&request)?,
            &Self::key("verify"),
        )
        .map_err(|error| anyhow!("{error}"))?;
        let result: aseman_contracts::vmm::VerificationResult = serde_json::from_str(&answer)?;
        Ok(if result.valid {
            json!({"ok": true, "security": result.security_level})
        } else {
            json!({"ok": false, "error": result.reason.unwrap_or_else(|| "the proof does not verify".to_owned())})
        }
        .to_string())
    }
}

/// The machine creature that owns `program` (legacy IDs).
pub(crate) fn program_machine(app: &Arc<dyn crate::models::core::ICore>, program: &str) -> String {
    let slot = Arc::new(std::sync::Mutex::new(String::new()));
    let out = slot.clone();
    let program = program.to_owned();
    app.modify_state(
        true,
        Box::new(move |trx: &dyn crate::models::transaction::ITrx| {
            *out.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                (crate::shell::api::model::program_ports::ProgramPorts { trx })
                    .program_or_empty(&program)
                    .machine_id;
            Ok(())
        }),
    );
    let machine = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    machine
}

impl RemoteWorkloads {
    /// A signal for `program` (the legacy run path, after its store-membership check):
    /// an invocation of the entity's signal workload. `entity` defaults to `main`, as
    /// legacy programs deploy their module there.
    pub(crate) fn signal(
        &self,
        app: &Arc<dyn crate::models::core::ICore>,
        program: &str,
        entity: &str,
        runtime: &str,
        store_id: &str,
        packet: Value,
    ) {
        let entity = if entity.is_empty() {
            aseman_domain::program::DEFAULT_ALARM_ENTITY
        } else {
            entity
        };
        let machine = program_machine(app, program);
        if let Err(error) = self.invoke(program, &machine, entity, runtime, store_id, packet, false)
        {
            eprintln!("signal to {program}/{entity} was not delivered: {error}");
        }
    }
}

/// The guest API as the node serves it.
struct NodeGuestApi {
    catalog: PostgresCapsuleRepository,
    blobs: StorageRootBlobStore,
    policy: VerifierPolicy,
}

impl GuestApi for NodeGuestApi {
    fn serve(
        &self,
        proof: &Proof,
        body: &[u8],
        request: GuestRequest<'_>,
    ) -> Result<Vec<u8>, IdentityFailure> {
        let action = match request {
            GuestRequest::Call { op, .. } => {
                call_action(op).ok_or(IdentityFailure::Refused("unregistered host call"))?
            }
            GuestRequest::Artifact { .. } => ARTIFACT_ACTION,
        };
        let workloads = CapsuleWorkloads {
            repository: &self.catalog,
        };
        ServeGuestCall {
            keys: &CapsuleKeyDirectory {
                repository: &self.catalog,
            },
            replay: &self.catalog,
            verifier: &NativeIdentityVerifier,
            clock: &SystemClock,
            workloads: &workloads,
            refs: &workloads,
            calls: self,
        }
        .execute(proof, body, request, action, &self.policy)
    }
}

impl GuestHostCalls for NodeGuestApi {
    fn call(&self, caller: &GuestCaller, op: &str, input: &str) -> PortResult<String> {
        let input: Value = serde_json::from_str(input)
            .map_err(|_| PortError::Failed("the input is not JSON".to_owned()))?;
        let (entity, instance) = caller.entity_and_instance();
        // Identity is stamped here from the resolved caller, exactly where the
        // runtimes stamp it for local VMs; the authorized host-call path takes it
        // from the packet, never from the input.
        let packet = json!({
            "type": "hostCall",
            "op": op,
            "input": input,
            "creatureId": caller.program_ref,
            "programId": caller.program_ref,
            "machineId": caller.program_ref,
            "entityId": entity,
            "vmId": instance,
        });
        Ok(crate::drivers::vmm::host::vm_host_functions::handle_unified_host_call(&packet))
    }

    fn artifact(&self, caller: &GuestCaller, digest: &str) -> PortResult<Vec<u8>> {
        let (entity, _) = caller.entity_and_instance();
        let store_key = CapsuleEntityPorts {
            repository: &self.catalog,
        }
        .artifact(&caller.program_ref, entity, ArtifactRole::Primary)?
        .and_then(|artifact| artifact.store_key)
        .ok_or(PortError::NotFound)?;
        let bytes = self.blobs.blob(&store_key)?.ok_or(PortError::NotFound)?;
        // Only the caller's own entity file, and only the version it was given.
        if format!("sha256:{}", hex::encode(Sha256::digest(&bytes))) != digest {
            return Err(PortError::NotFound);
        }
        Ok(bytes)
    }
}

fn certificates(path: &str) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let bytes = std::fs::read(path)?;
    rustls_pemfile::certs(&mut BufReader::new(bytes.as_slice()))
        .collect::<Result<_, _>>()
        .map_err(Into::into)
}

/// Connect to the VMM (the node process, and the operator's handoff command).
pub(crate) fn connect(
    config: &VmmClientConfig,
    node_id: &str,
    database_url: &str,
    storage_root: &str,
) -> Result<RemoteWorkloads> {
    let identity = aseman_config::read_secret_file(&config.identity_secret, 64 * 1024)?;
    let client = HttpVmmClient::new(
        &config.endpoint,
        &ClientTls {
            server_roots_pem: std::fs::read(&config.server_ca)?,
            identity_pem: identity.into_bytes(),
        },
        node_id,
        Duration::from_millis(config.deadline_millis),
    )
    .map_err(|error| anyhow!("{error}"))?;
    Ok(RemoteWorkloads {
        client,
        runtimes: std::sync::Mutex::new(None),
        catalog: PostgresCapsuleRepository::connect(database_url)?,
        blobs: StorageRootBlobStore::new(storage_root),
        guest_api_url: config.guest_api_url.clone(),
        audience: audience(&node_subject(node_id)),
    })
}

/// Connect to the VMM and start the guest API (called once at startup, after core
/// storage is on PostgreSQL).
pub(crate) fn install(
    config: &VmmClientConfig,
    node_id: &str,
    database_url: &str,
    storage_root: &str,
) -> Result<()> {
    let remote = connect(config, node_id, database_url, storage_root)?;
    let chain = certificates(&config.guest_api_certificate)?;
    let key_pem = aseman_config::read_secret_file(&config.guest_api_key_secret, 64 * 1024)?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(key_pem.as_bytes()))?
        .ok_or_else(|| anyhow!("the guest API key secret holds no private key"))?;
    let api = Arc::new(NodeGuestApi {
        catalog: PostgresCapsuleRepository::connect(database_url)?,
        blobs: StorageRootBlobStore::new(storage_root),
        policy: VerifierPolicy {
            audience: remote.audience.clone(),
            freshness: FreshnessPolicy::GUEST,
            rotation: RotationPolicy::DEFAULT,
        },
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let listener = runtime.block_on(tokio::net::TcpListener::bind(&config.guest_api_listen))?;
    std::thread::Builder::new()
        .name("guest-api".to_owned())
        .spawn(move || {
            if let Err(error) = runtime.block_on(aseman_guest_http::server::serve(
                listener,
                chain,
                key,
                api,
                std::future::pending(),
            )) {
                eprintln!("the guest API stopped: {error}");
            }
        })?;
    REMOTE
        .set(Arc::new(remote))
        .map_err(|_| anyhow!("the remote VMM is already installed"))
}

/// The machine creature that owns `program`, from the PostgreSQL catalog.
fn catalog_program_machine(catalog: &PostgresCapsuleRepository, program: &str) -> Result<String> {
    aseman_ports::ProgramDirectory::program(
        &aseman_capsule::program::CapsuleProgramPorts {
            repository: catalog,
        },
        program,
    )
    .map_err(|error| anyhow!("{error}"))?
    .map(|record| record.machine_id)
    .ok_or_else(|| anyhow!("program {program} does not exist"))
}

/// `aseman-node vmm-handoff plan OUT` / `aseman-node vmm-handoff apply PLAN DECISIONS`
/// (ADR 0022, P5-05), run while the node is stopped.
///
/// `plan` writes the legacy VM handoff plan (instances, external handles, digest) as
/// JSON. `apply` checks the decisions against that plan (its digest must still
/// match the store), runs every adopted instance as a workload of the configured VMM,
/// and only then removes the decided observed records from the legacy store.
pub(crate) fn handoff(config: &aseman_config::AsemanConfig, arguments: &[String]) -> Result<()> {
    let legacy = aseman_storage_legacy::RocksDbKvStore::open_tuned(std::path::Path::new(
        &config.storage.base_db_path,
    ))
    .map_err(|error| anyhow!("{error}"))?;
    match arguments {
        [command, out] if command == "plan" => {
            let plan = aseman_storage_legacy::plan_legacy_vm_handoff(&legacy)
                .map_err(|error| anyhow!("{error}"))?;
            std::fs::write(out, serde_json::to_vec_pretty(&plan)?)?;
            println!(
                "{} instances, {} external handles; digest {}",
                plan.instances.len(),
                plan.external.len(),
                hex::encode(plan.digest)
            );
            Ok(())
        }
        [command, plan_path, decisions_path] if command == "apply" => {
            let approved: aseman_storage_legacy::LegacyVmHandoffPlan =
                serde_json::from_slice(&std::fs::read(plan_path)?)?;
            let decisions: aseman_storage_legacy::LegacyVmHandoffDecisions =
                serde_json::from_slice(&std::fs::read(decisions_path)?)?;
            let current = aseman_storage_legacy::plan_legacy_vm_handoff(&legacy)
                .map_err(|error| anyhow!("{error}"))?;
            if current.digest != approved.digest {
                return Err(anyhow!(
                    "the legacy VM handoff plan changed since it was approved; plan again"
                ));
            }
            aseman_storage_legacy::check_legacy_vm_decisions(&current, &decisions)
                .map_err(|error| anyhow!("{error}"))?;
            let vmm = config
                .vmm
                .as_ref()
                .ok_or_else(|| anyhow!("adoption needs ASEMAN_VMM_ENDPOINT"))?;
            let database_url = aseman_config::read_secret_file(
                config
                    .database_url_secret
                    .as_deref()
                    .ok_or_else(|| anyhow!("ASEMAN_DATABASE_URL_SECRET is required"))?,
                4096,
            )?;
            let remote = connect(
                vmm,
                &config.node.id,
                &database_url,
                &config.storage.root_path,
            )?;
            for instance in &current.instances {
                if decisions.instances.get(&instance.key())
                    != Some(&aseman_storage_legacy::LegacyVmDecision::Adopt)
                {
                    continue;
                }
                if instance.runtime.is_empty() {
                    return Err(anyhow!(
                        "{} has no recorded runtime; decide stop instead",
                        instance.key()
                    ));
                }
                let machine = catalog_program_machine(&remote.catalog, &instance.program)?;
                let workload = remote.launch(
                    &instance.program,
                    &machine,
                    &instance.entity,
                    &instance.vm,
                    &instance.runtime,
                    LaunchResources::default(),
                    BTreeMap::new(),
                )?;
                println!("adopted {} as workload {workload}", instance.key());
            }
            let removed = aseman_storage_legacy::complete_legacy_vm_handoff(
                &legacy,
                approved.digest,
                &decisions,
            )
            .map_err(|error| anyhow!("{error}"))?;
            println!("removed {removed} legacy observed records");
            Ok(())
        }
        _ => Err(anyhow!(
            "usage: vmm-handoff plan OUT | vmm-handoff apply PLAN DECISIONS"
        )),
    }
}
