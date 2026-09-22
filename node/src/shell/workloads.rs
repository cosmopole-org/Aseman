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

use anyhow::{anyhow, Result};
use aseman_application::guest_call::{
    GuestRequest, ProvisionWorkload, ServeGuestCall, WorkloadKey,
};
use aseman_application::identity::{IdentityFailure, VerifierPolicy};
use aseman_application::SetDesiredWorkloadState;
use aseman_capsule_repositories::capability::CapsuleGrantStore;
use aseman_capsule_repositories::entity::CapsuleEntityPorts;
use aseman_capsule_repositories::identity::CapsuleKeyDirectory;
use aseman_capsule_repositories::workload::CapsuleWorkloads;
use aseman_config::VmmClientConfig;
use aseman_contracts::guest_api::{audience, call_action, WorkloadCredential, ARTIFACT_ACTION};
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
use serde_json::{json, Value};
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
        .execute(
            Subject {
                kind: SubjectKind::User,
                id: capsule("Creature", user),
            },
            workload,
            state,
            &BTreeSet::from([Condition::Owner]),
        )
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

/// The machine creature that owns `program` (legacy IDs).
fn program_machine(app: &Arc<dyn crate::models::core::ICore>, program: &str) -> String {
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
        &aseman_capsule_repositories::program::CapsuleProgramPorts {
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
