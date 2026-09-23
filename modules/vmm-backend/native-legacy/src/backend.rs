//! [`VmmBackend`] over the Caspar runtime plugins (P5-03).
//!
//! The backend turns A504 requests into the plugins' legacy packets. Workloads are
//! identified to the plugins by the legacy machine and VM identifiers the node set as
//! labels, and every call a plugin makes back goes through [`NativeHost`], as the
//! workload. Program artifacts are fetched from the node with the workload's
//! credential and verified against their digest before use.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use aseman_contracts::guest_api::WorkloadCredential;
use aseman_contracts::vmm::{
    BuildRequest, ExecRequest, ExecResult, HttpRequest, HttpResponse, Invocation, InvocationResult,
    VerificationRequest,
};
use aseman_domain::vmm::{
    ArtifactKind, DeployConventions, Endpoint, LogRecord, Observation, OperationKind,
    OperationRecord, PortProtocol, ReconcileAction, RuntimeCapabilities, Usage, WorkloadRecord,
};
use aseman_domain::{ObservedWorkloadState, WorkloadId};
use aseman_guest_http::client::GuestApiClient;
use aseman_ports::vmm::{BackendDescription, VmmBackend};
use aseman_ports::{PortError, PortResult};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use caspar_vm_sdk::VmPlugin;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::host::{NativeHost, now_millis};
use crate::registry::{Instance, PluginState, Registry};

/// The capabilities A505 derives for each native runtime from its A006 behavior.
const PARITY_JSON: &str = include_str!("../../../../docs/generated/vmm-native-parity.json");

pub struct NativeBackend {
    registry: Registry,
    /// Where docker containers make their host calls and receive signals.
    gateway: Arc<crate::docker_host::DockerHostGateway>,
    state: Arc<PluginState>,
    guest: Arc<GuestApiClient>,
    artifacts: PathBuf,
    capabilities: Vec<RuntimeCapabilities>,
}

fn failed(error: impl std::fmt::Display) -> PortError {
    PortError::Failed(error.to_string())
}

/// The runtimes A505 declares, as `RuntimeCapabilities`.
///
/// # Errors
///
/// When the generated parity matrix is malformed.
pub fn declared_capabilities() -> PortResult<Vec<RuntimeCapabilities>> {
    let parity: Value = serde_json::from_str(PARITY_JSON).map_err(failed)?;
    let runtimes = parity["runtimes"]
        .as_object()
        .ok_or_else(|| failed("the parity matrix has no runtimes"))?;
    runtimes
        .iter()
        .map(|(key, flags)| {
            let flag = |name: &str| flags[name].as_bool().unwrap_or(false);
            Ok(RuntimeCapabilities {
                runtime: key.clone(),
                invocation: flag("invocation"),
                long_running: flag("long_running"),
                pause: flag("pause"),
                snapshot: flag("snapshot"),
                exec: flag("exec"),
                terminal: flag("terminal"),
                http_ingress: flag("http_ingress"),
                files: flag("files"),
                build: flag("build"),
                chain_transactions: flag("chain_transactions"),
                execution_proofs: flag("execution_proofs"),
                deploy: serde_json::from_value::<DeployConventions>(flags["deploy"].clone())
                    .map_err(failed)?,
            })
        })
        .collect()
}

impl NativeBackend {
    /// Register the plugins with a host that reaches the node only through `guest`,
    /// and keep plugin state and artifacts under `state_dir`. One backend per process:
    /// the plugins have one host.
    ///
    /// # Errors
    ///
    /// When the state directory cannot be prepared.
    pub fn start(
        state_dir: PathBuf,
        guest: GuestApiClient,
        docker_gateway_port: Option<u16>,
    ) -> PortResult<Self> {
        let artifacts = state_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).map_err(failed)?;
        let registry = Registry::default();
        let state = Arc::new(PluginState::open(state_dir.join("plugin-state.json")));
        let guest = Arc::new(guest);
        let host: Arc<NativeHost> = Arc::new(NativeHost {
            registry: registry.clone(),
            state: state.clone(),
            guest: guest.clone(),
            storage_root: format!("{}/", state_dir.display()),
        });
        caspar_vm_sdk::set_host(host.clone());
        caspar_vm_plugins::register_all();
        let registered = caspar_vm_sdk::registry::keys();
        let capabilities = declared_capabilities()?
            .into_iter()
            .filter(|runtime| registered.contains(&runtime.runtime))
            .collect();
        // A container is identified by its docker-network source IP: the owning
        // plugin names the container, and the container name was registered for
        // one workload when the plugin launched it. The container declares nothing.
        let identify_registry = registry.clone();
        let gateway = crate::docker_host::DockerHostGateway::new(
            Box::new(move |ip| {
                let name = caspar_vm_sdk::registry::plugins()
                    .into_iter()
                    .find_map(|plugin| plugin.identify_instance_by_ip(ip))?;
                identify_registry.container_identity(&name)
            }),
            host,
        );
        if let Some(port) = docker_gateway_port {
            gateway.listen(i64::from(port));
        }
        Ok(Self {
            registry,
            gateway,
            state,
            guest,
            artifacts,
            capabilities,
        })
    }

    fn runtime(&self, key: &str) -> PortResult<(RuntimeCapabilities, Arc<dyn VmPlugin>)> {
        let capabilities = self
            .capabilities
            .iter()
            .find(|runtime| runtime.runtime == key)
            .cloned()
            .ok_or(PortError::Unsupported("runtime"))?;
        let plugin = caspar_vm_sdk::registry::get(key).ok_or(PortError::Unsupported("runtime"))?;
        Ok((capabilities, plugin))
    }

    fn credential(workload: &WorkloadRecord) -> PortResult<WorkloadCredential> {
        let text = workload
            .spec
            .bootstrap
            .credential
            .as_ref()
            .ok_or(PortError::Denied("the workload has no credential"))?;
        WorkloadCredential::decode(text.expose())
            .map_err(|_| PortError::Denied("invalid workload credential"))
    }

    /// Fetch and verify the program artifact into the cache; returns its path.
    fn artifact(
        &self,
        workload: &WorkloadRecord,
        credential: &WorkloadCredential,
        deploy: &DeployConventions,
    ) -> PortResult<PathBuf> {
        let artifact = &workload.spec.artifact;
        if artifact.kind != ArtifactKind::Blob {
            return Err(PortError::Unsupported(
                "OCI artifacts on the native backend",
            ));
        }
        let hex = artifact
            .digest
            .strip_prefix("sha256:")
            .filter(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| failed("invalid artifact digest"))?;
        let directory = self.artifacts.join(hex);
        let path = directory.join(&deploy.entity_file_name);
        if path.exists() {
            return Ok(path);
        }
        let bytes = self.guest.artifact(credential, &artifact.digest)?;
        let actual: [u8; 32] = Sha256::digest(&bytes).into();
        let actual: String = actual.iter().map(|byte| format!("{byte:02x}")).collect();
        if actual != hex {
            return Err(failed("the artifact does not match its digest"));
        }
        std::fs::create_dir_all(&directory).map_err(failed)?;
        let temporary = directory.join(format!(".{}.tmp", deploy.entity_file_name));
        std::fs::write(&temporary, &bytes).map_err(failed)?;
        std::fs::rename(&temporary, &path).map_err(failed)?;
        Ok(path)
    }

    /// The workload's instance, created (artifact fetched, identity registered) on
    /// first use; later calls refresh its record.
    fn ensure(&self, workload: &WorkloadRecord) -> PortResult<()> {
        if let Some(build_pending) = self.registry.with(workload.id, |instance| {
            instance.record = workload.clone();
            instance.build_pending
        }) {
            return if build_pending {
                // The previous build failed; its output is in the instance's log.
                self.build_image(workload)
            } else {
                Ok(())
            };
        }
        let (capabilities, _) = self.runtime(&workload.spec.runtime)?;
        let credential = Self::credential(workload)?;
        if credential.subject.id != *workload.id.as_uuid() {
            return Err(PortError::Denied("the credential is another workload's"));
        }
        let artifact_path = self.artifact(workload, &credential, &capabilities.deploy)?;
        let id_text = workload.id.to_string();
        let machine_id = workload
            .labels
            .legacy_machine_id
            .clone()
            .unwrap_or_else(|| id_text.clone());
        let vm_id = workload
            .labels
            .legacy_vm_id
            .clone()
            .unwrap_or_else(|| id_text.clone());
        let entity_id = workload.labels.entity_id.clone();
        // Where the plugins' launch plans look for the entity's module.
        self.state
            .apply(&[(
                format!("vmEntityPath::{machine_id}::{entity_id}"),
                Some(artifact_path.display().to_string()),
            )])
            .map_err(failed)?;
        self.registry.insert(Instance {
            record: workload.clone(),
            credential: Arc::new(credential),
            machine_id,
            vm_id,
            entity_id,
            artifact_path,
            build_pending: capabilities.deploy.build_on_deploy,
            observation: Observation {
                state: ObservedWorkloadState::Pending,
                generation: workload.desired.generation,
                sequence: self.registry.next_sequence(),
                reason: None,
                observed_at_millis: now_millis(),
            },
            logs: std::collections::VecDeque::new(),
            next_log: 0,
            usage: Usage::default(),
        });
        if capabilities.deploy.build_on_deploy {
            self.build_image(workload)?;
        }
        Ok(())
    }

    /// Build a build-on-deploy runtime's image (docker, elpify) from the fetched
    /// artifact's directory, once, before the workload's first start. The plugin's
    /// output is the workload's `build` log stream.
    fn build_image(&self, workload: &WorkloadRecord) -> PortResult<()> {
        let (_, plugin) = self.runtime(&workload.spec.runtime)?;
        let (machine_id, vm_id, entity_id, artifact_path) = self.identity(workload.id)?;
        let directory = std::path::Path::new(&artifact_path)
            .parent()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        plugin
            .build_image(&json!({
                "type": "buildVmImage",
                "runtime": workload.spec.runtime,
                "buildType": workload.spec.runtime,
                "machineId": machine_id,
                "entityId": entity_id,
                "vmId": vm_id,
                "imageBuildPath": directory,
                "astPath": artifact_path,
            }))
            .map_err(|error| failed(format!("build: {error}")))?;
        self.registry
            .with(workload.id, |instance| instance.build_pending = false);
        Ok(())
    }

    fn identity(&self, id: WorkloadId) -> PortResult<(String, String, String, String)> {
        self.registry
            .with(id, |instance| {
                (
                    instance.machine_id.clone(),
                    instance.vm_id.clone(),
                    instance.entity_id.clone(),
                    instance.artifact_path.display().to_string(),
                )
            })
            .ok_or(PortError::NotFound)
    }

    fn observe(
        &self,
        workload: &WorkloadRecord,
        state: ObservedWorkloadState,
        reason: Option<String>,
    ) -> Observation {
        let observation = Observation {
            state,
            generation: workload.desired.generation,
            sequence: self.registry.next_sequence(),
            reason,
            observed_at_millis: now_millis(),
        };
        self.registry.with(workload.id, |instance| {
            instance.observation = observation.clone()
        });
        observation
    }

    fn context(&self, workload: &WorkloadRecord) -> PortResult<Value> {
        let (machine_id, vm_id, entity_id, _) = self.identity(workload.id)?;
        Ok(json!({
            "machineId": machine_id,
            "programId": machine_id,
            "creatureId": machine_id,
            "entityId": entity_id,
            "vmId": vm_id,
            "resources": {
                "cpuMillis": workload.spec.resources.vcpu_millis,
                "ramMb": workload.spec.resources.memory_mib,
                "diskMb": workload.spec.resources.disk_mib.unwrap_or(0),
            },
            "params": workload.spec.environment,
        }))
    }

    /// Resolve a stop or delete plan's state links from plugin state.
    fn resolved(&self, plan: &Value) -> PortResult<Value> {
        let mut input = plan["input"].clone();
        for link in plan["links"].as_array().into_iter().flatten() {
            let field = link["field"].as_str().unwrap_or("");
            let value = self.state.get(link["key"].as_str().unwrap_or(""));
            if value.is_empty() && link["required"].as_bool().unwrap_or(false) {
                return Err(failed(format!("the runtime state {field} is missing")));
            }
            if !field.is_empty() {
                input[field] = Value::String(value);
            }
        }
        Ok(input)
    }

    fn launch(&self, workload: &WorkloadRecord) -> PortResult<Observation> {
        self.ensure(workload)?;
        let (capabilities, plugin) = self.runtime(&workload.spec.runtime)?;
        if !capabilities.long_running {
            // An invocation runtime is ready once its module is in place.
            return Ok(self.observe(workload, ObservedWorkloadState::Running, None));
        }
        let plan = plugin
            .plan_run_entity(&self.context(workload)?)
            .map_err(failed)?;
        let links: Vec<(String, Option<String>)> = plan["links"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|pair| {
                Some((
                    pair[0].as_str()?.to_owned(),
                    Some(pair[1].as_str()?.to_owned()),
                ))
            })
            .collect();
        self.state.apply(&links).map_err(failed)?;
        let mut packet = plan["input"].clone();
        packet["type"] = json!("runVm");
        packet["astPath"] = json!(self.identity(workload.id)?.3);
        Ok(match plugin.run_vm(&packet) {
            Ok(_) => self.observe(workload, ObservedWorkloadState::Running, None),
            Err(error) => self.observe(workload, ObservedWorkloadState::Failed, Some(error)),
        })
    }

    fn stop(&self, workload: &WorkloadRecord, delete: bool) -> PortResult<Observation> {
        let stopped = Observation {
            state: ObservedWorkloadState::Stopped,
            generation: workload.desired.generation,
            sequence: self.registry.next_sequence(),
            reason: None,
            observed_at_millis: now_millis(),
        };
        if !self.registry.contains(workload.id) {
            return Ok(stopped);
        }
        let (capabilities, plugin) = self.runtime(&workload.spec.runtime)?;
        if capabilities.long_running {
            let context = self.context(workload)?;
            let plan = if delete {
                plugin.plan_delete_entity(&context)
            } else {
                plugin.plan_stop_entity(&context)
            }
            .map_err(failed)?;
            let mut packet = self.resolved(&plan)?;
            packet["type"] = json!(if delete { "deleteVm" } else { "terminateVm" });
            if delete {
                plugin.delete_vm(&packet).map_err(failed)?;
            } else {
                plugin.terminate_vm(&packet).map_err(failed)?;
            }
        }
        if delete {
            self.registry.remove(workload.id);
            Ok(stopped)
        } else {
            Ok(self.observe(workload, ObservedWorkloadState::Stopped, None))
        }
    }

    /// A long-running instance takes its signals over the docker-host gateway: pushed
    /// to the live container, or queued until it connects, starting it when no start
    /// is already under way (the legacy delivery).
    fn deliver(&self, workload: &WorkloadRecord, invocation: &Invocation) -> PortResult<String> {
        let (machine_id, _, entity_id, _) = self.identity(workload.id)?;
        let reached = self.gateway.push_signal_to_entity(
            &machine_id,
            &entity_id,
            &invocation.key,
            &invocation.payload,
        );
        if reached == 0 {
            self.gateway.queue_pending_signal(
                &machine_id,
                &entity_id,
                &invocation.key,
                &invocation.payload,
            );
            if self.gateway.begin_cold_spawn(&machine_id, &entity_id)
                && self.registry.running_state(workload.id) != Some(ObservedWorkloadState::Running)
            {
                self.launch(workload)?;
            }
        }
        self.registry.with(workload.id, |instance| {
            instance.usage.invocations += 1;
            instance.usage.sequence += 1;
            instance.usage.window_end_millis = now_millis();
        });
        serde_json::to_string(&InvocationResult {
            output: Some(json!({"delivered": reached, "queued": reached == 0})),
            gas_used: None,
            proof: None,
        })
        .map_err(failed)
    }

    fn invoke(&self, workload: &WorkloadRecord, request: &str) -> PortResult<String> {
        let invocation: Invocation = serde_json::from_str(request).map_err(failed)?;
        self.ensure(workload)?;
        let (capabilities, plugin) = self.runtime(&workload.spec.runtime)?;
        if capabilities.long_running
            && capabilities.http_ingress
            && workload.spec.runtime == "docker"
        {
            return self.deliver(workload, &invocation);
        }
        let (machine_id, vm_id, entity_id, ast_path) = self.identity(workload.id)?;
        let input = match &invocation.payload {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let packet = json!({
            "type": "runVm",
            "runtime": workload.spec.runtime,
            "vmType": workload.spec.runtime,
            "machineId": machine_id,
            "vmId": vm_id,
            "entityId": entity_id,
            "storeId": invocation.store_id,
            "key": invocation.key,
            "input": input,
            "astPath": ast_path,
        });
        let started = Instant::now();
        let outcome = plugin.run_vm(&packet);
        let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.registry.with(workload.id, |instance| {
            instance.usage.invocations += 1;
            instance.usage.cpu_millis += elapsed;
            instance.usage.sequence += 1;
            instance.usage.window_end_millis = now_millis();
        });
        let output = outcome.map_err(failed)?;
        serde_json::to_string(&InvocationResult {
            output: Some(output),
            gas_used: None,
            proof: None,
        })
        .map_err(failed)
    }

    fn exec(&self, workload: &WorkloadRecord, request: &str) -> PortResult<String> {
        let request: ExecRequest = serde_json::from_str(request).map_err(failed)?;
        self.ensure(workload)?;
        let (_, plugin) = self.runtime(&workload.spec.runtime)?;
        let (machine_id, vm_id, entity_id, _) = self.identity(workload.id)?;
        let answer = plugin
            .exec_vm(&json!({
                "type": "execVm",
                "machineId": machine_id,
                "vmId": vm_id,
                "entityId": entity_id,
                "command": request.command.join(" "),
                "args": request.command,
            }))
            .map_err(failed)?;
        let text = |name: &str| {
            answer[name]
                .as_str()
                .map_or_else(String::new, |text| URL_SAFE_NO_PAD.encode(text))
        };
        serde_json::to_string(&ExecResult {
            exit_code: answer["exitCode"]
                .as_i64()
                .and_then(|code| i32::try_from(code).ok())
                .unwrap_or(0),
            stdout: match (answer["stdout"].as_str(), answer["output"].as_str()) {
                (Some(_), _) => text("stdout"),
                (None, Some(output)) => URL_SAFE_NO_PAD.encode(output),
                (None, None) => URL_SAFE_NO_PAD.encode(answer.to_string()),
            },
            stderr: text("stderr"),
            truncated: false,
        })
        .map_err(failed)
    }

    fn build(&self, request: &str) -> PortResult<String> {
        let request: BuildRequest = serde_json::from_str(request).map_err(failed)?;
        let (_, plugin) = self.runtime(&request.runtime)?;
        // A build runs for a workload of the same program and entity, whose
        // credential fetched the sources.
        let workload = self
            .registry
            .find(|instance| {
                instance.record.labels.program_id == request.labels.program_id
                    && instance.record.labels.entity_id == request.labels.entity_id
            })
            .ok_or_else(|| failed("no workload of this entity runs here to build for"))?;
        let (machine_id, _, entity_id, ast_path) = self.identity(workload)?;
        let build_path = std::path::Path::new(&ast_path)
            .parent()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        plugin
            .build_image(&json!({
                "type": "buildVmImage",
                "runtime": request.runtime,
                "machineId": machine_id,
                "entityId": entity_id,
                "imageBuildPath": build_path,
                "buildType": request.build_type.unwrap_or_else(|| request.runtime.clone()),
            }))
            .map_err(failed)?;
        let artifact = self
            .registry
            .with(workload, |instance| instance.record.spec.artifact.clone())
            .ok_or(PortError::NotFound)?;
        Ok(json!({"artifact": artifact}).to_string())
    }
}

impl VmmBackend for NativeBackend {
    fn describe(&self) -> PortResult<BackendDescription> {
        Ok(BackendDescription {
            name: "native-legacy".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            contract: aseman_vmm_backend_grpc::convert::CONTRACT.to_owned(),
            runtimes: self.capabilities.clone(),
        })
    }

    fn step(&self, workload: &WorkloadRecord, action: ReconcileAction) -> PortResult<Observation> {
        match action {
            ReconcileAction::Start | ReconcileAction::Resume => self.launch(workload),
            ReconcileAction::Restart => {
                self.stop(workload, false)?;
                self.launch(workload)
            }
            ReconcileAction::Stop => self.stop(workload, false),
            ReconcileAction::Delete => self.stop(workload, true),
            ReconcileAction::Pause => Err(PortError::Unsupported("pause")),
            ReconcileAction::None | ReconcileAction::Adopt => {
                Err(failed("not a reconciliation step"))
            }
        }
    }

    fn observe_all(&self) -> PortResult<Vec<(WorkloadId, Observation)>> {
        Ok(self.registry.observations())
    }

    fn run(
        &self,
        workload: Option<&WorkloadRecord>,
        operation: &OperationRecord,
    ) -> PortResult<String> {
        let request = operation.request.as_deref().unwrap_or("{}");
        match (operation.kind, workload) {
            (OperationKind::Invoke, Some(workload)) => self.invoke(workload, request),
            (OperationKind::Exec, Some(workload)) => self.exec(workload, request),
            (OperationKind::Build, _) => self.build(request),
            (OperationKind::Snapshot | OperationKind::Restore, _) => {
                Err(PortError::Unsupported("snapshots"))
            }
            _ => Err(failed("not a data-plane operation")),
        }
    }

    fn forward_http(&self, workload: &WorkloadRecord, request: &str) -> PortResult<String> {
        let request: HttpRequest = serde_json::from_str(request).map_err(failed)?;
        self.ensure(workload)?;
        let (_, plugin) = self.runtime(&workload.spec.runtime)?;
        let (machine_id, vm_id, entity_id, ast_path) = self.identity(workload.id)?;
        let body = request
            .body
            .as_deref()
            .map(|body| URL_SAFE_NO_PAD.decode(body))
            .transpose()
            .map_err(failed)?
            .unwrap_or_default();
        let answer = plugin
            .forward_http(&json!({
                "type": "forwardHttp",
                "creatureId": machine_id,
                "programId": machine_id,
                "machineId": machine_id,
                "entityId": entity_id,
                "vmId": vm_id,
                "vmType": workload.spec.runtime,
                "astPath": ast_path,
                "method": request.method,
                "path": request.path,
                "query": request.query.unwrap_or_default(),
                "headers": request.headers,
                "bodyBase64": STANDARD.encode(body),
            }))
            .map_err(failed)?;
        let body = match (answer["bodyBase64"].as_str(), answer["body"].as_str()) {
            (Some(encoded), _) => STANDARD.decode(encoded).map_err(failed)?,
            (None, Some(text)) => text.as_bytes().to_vec(),
            (None, None) => Vec::new(),
        };
        serde_json::to_string(&HttpResponse {
            status: answer["status"]
                .as_u64()
                .and_then(|status| u16::try_from(status).ok())
                .unwrap_or(502),
            headers: answer["headers"]
                .as_object()
                .map(|headers| {
                    headers
                        .iter()
                        .filter_map(|(name, value)| {
                            Some((name.clone(), value.as_str()?.to_owned()))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            body: Some(URL_SAFE_NO_PAD.encode(body)),
        })
        .map_err(failed)
    }

    fn put_file(&self, workload: &WorkloadRecord, path: &str, bytes: &[u8]) -> PortResult<()> {
        let (capabilities, plugin) = self.runtime(&workload.spec.runtime)?;
        if !capabilities.files {
            return Err(PortError::Unsupported("files"));
        }
        self.ensure(workload)?;
        let (machine_id, vm_id, entity_id, _) = self.identity(workload.id)?;
        let (directory, name) = path.rsplit_once('/').unwrap_or(("", path));
        plugin
            .copy_to_vm(&json!({
                "type": "copyToVm",
                "machineId": machine_id,
                "vmId": vm_id,
                "entityId": entity_id,
                "path": path,
                "targetPath": directory,
                "fileName": name,
                // The legacy runtimes copy text content.
                "content": std::str::from_utf8(bytes)
                    .map_err(|_| PortError::Unsupported("binary files on the native backend"))?,
            }))
            .map(|_| ())
            .map_err(failed)
    }

    fn get_file(&self, workload: &WorkloadRecord, path: &str) -> PortResult<Vec<u8>> {
        let (capabilities, plugin) = self.runtime(&workload.spec.runtime)?;
        if !capabilities.files {
            return Err(PortError::Unsupported("files"));
        }
        let Ok((machine_id, vm_id, entity_id, _)) = self.identity(workload.id) else {
            return Err(PortError::NotFound);
        };
        let answer = plugin
            .copy_from_vm(&json!({
                "type": "copyFromVm",
                "machineId": machine_id,
                "vmId": vm_id,
                "entityId": entity_id,
                "path": path,
                "sourcePath": path,
            }))
            .map_err(|_| PortError::NotFound)?;
        match (answer["contentBase64"].as_str(), answer["content"].as_str()) {
            (Some(encoded), _) | (None, Some(encoded)) => STANDARD.decode(encoded).map_err(failed),
            (None, None) => Err(PortError::NotFound),
        }
    }

    fn endpoints(&self, workload: &WorkloadRecord) -> PortResult<Vec<Endpoint>> {
        let (_, plugin) = self.runtime(&workload.spec.runtime)?;
        let Ok((machine_id, vm_id, entity_id, _)) = self.identity(workload.id) else {
            return Ok(Vec::new());
        };
        let answer = plugin
            .vm_endpoints(&json!({"machineId": machine_id, "vmId": vm_id, "entityId": entity_id}))
            .unwrap_or(Value::Null);
        Ok(answer["endpoints"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|endpoint| {
                Some(Endpoint {
                    name: endpoint["name"].as_str().unwrap_or("http").to_owned(),
                    protocol: if endpoint["protocol"] == "tcp" {
                        PortProtocol::Tcp
                    } else {
                        PortProtocol::Http
                    },
                    address: endpoint["address"]
                        .as_str()
                        .or_else(|| endpoint["url"].as_str())?
                        .to_owned(),
                    port: u16::try_from(endpoint["port"].as_u64().unwrap_or(0)).ok()?,
                })
            })
            .collect())
    }

    fn usage(&self, workload: &WorkloadRecord) -> PortResult<Usage> {
        Ok(self
            .registry
            .with(workload.id, |instance| instance.usage)
            .unwrap_or_default())
    }

    fn logs(
        &self,
        workload: &WorkloadRecord,
        after: u64,
        limit: usize,
    ) -> PortResult<Vec<LogRecord>> {
        Ok(self
            .registry
            .with(workload.id, |instance| {
                instance
                    .logs
                    .iter()
                    .filter(|record| record.sequence > after)
                    .take(limit)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    fn verify(&self, runtime: &str, request: &str) -> PortResult<String> {
        let (capabilities, plugin) = self.runtime(runtime)?;
        if !capabilities.execution_proofs {
            return Err(PortError::Unsupported("execution proofs"));
        }
        let request: VerificationRequest = serde_json::from_str(request).map_err(failed)?;
        // The program must be one this backend holds (verified by digest on fetch).
        let hex = request
            .program
            .digest
            .strip_prefix("sha256:")
            .ok_or_else(|| failed("invalid program digest"))?;
        let program = self
            .artifacts
            .join(hex)
            .join(&capabilities.deploy.entity_file_name);
        if !program.exists() {
            return Err(PortError::NotFound);
        }
        let proof = URL_SAFE_NO_PAD.decode(&request.proof).map_err(failed)?;
        let answer = plugin.verify_program_execution(&json!({
            "masmPath": program.display().to_string(),
            "inputs": request.inputs,
            "outputs": request.outputs,
            "proof": proof,
        }));
        Ok(match answer {
            Ok(value) => json!({
                "valid": true,
                "security_level": value["security"].as_u64(),
            }),
            Err(reason) => json!({"valid": false, "reason": reason}),
        }
        .to_string())
    }
}
