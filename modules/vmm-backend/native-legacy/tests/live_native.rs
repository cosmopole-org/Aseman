//! The native backend running the real JavaScript runtime. A guest API double serves
//! the program artifact and the host calls over real TLS, checking every request's
//! A401 signature, registered action, and resource like the node does.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aseman_application::guest_call::GuestRequest;
use aseman_application::identity::IdentityFailure;
use aseman_contracts::guest_api::{ARTIFACT_ACTION, WorkloadCredential, audience, call_action};
use aseman_contracts::identity::{PublicKey, body_digest, verify_proof_signature};
use aseman_domain::identity::{AuthenticationError, Proof, Subject, SubjectKind};
use aseman_domain::vmm::{
    Artifact, ArtifactKind, OperationKind, OperationRecord, ReconcileAction, WorkloadRecord,
    WriteOnlyCredential,
};
use aseman_domain::{Generation, ObservedWorkloadState, OperationId, OperationState, WorkloadId};
use aseman_guest_http::client::GuestApiClient;
use aseman_guest_http::server::{GuestApi, serve};
use aseman_ports::PortError;
use aseman_ports::conformance::vmm::sample_workload;
use aseman_ports::vmm::VmmBackend;
use aseman_vmm_backend_conformance::check_backend;
use aseman_vmm_backend_native::backend::NativeBackend;
use rcgen::{CertificateParams, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

const COUNTER: &[u8] = include_bytes!("../../../../vms/javascript/examples/counter.js");

#[derive(Default)]
struct Node {
    keys: Mutex<BTreeMap<String, PublicKey>>,
    documents: Mutex<BTreeMap<String, serde_json::Value>>,
    calls: Mutex<Vec<(Subject, String)>>,
}

fn digest(bytes: &[u8]) -> String {
    let hash: [u8; 32] = Sha256::digest(bytes).into();
    format!(
        "sha256:{}",
        hash.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

impl GuestApi for Node {
    fn serve(
        &self,
        proof: &Proof,
        body: &[u8],
        request: GuestRequest<'_>,
    ) -> Result<Vec<u8>, IdentityFailure> {
        let key = self
            .keys
            .lock()
            .unwrap()
            .get(&proof.key_id)
            .cloned()
            .ok_or(AuthenticationError::UnknownKey)?;
        verify_proof_signature(proof, &key)?;
        if proof.body_digest != body_digest(body) {
            return Err(AuthenticationError::BodyDigestMismatch.into());
        }
        let refused = IdentityFailure::Refused("the credential does not cover this operation");
        match request {
            GuestRequest::Artifact { digest: wanted } => {
                if proof.action != ARTIFACT_ACTION || proof.resource != wanted {
                    return Err(refused);
                }
                if wanted == digest(COUNTER) {
                    Ok(COUNTER.to_vec())
                } else {
                    Err(IdentityFailure::Unavailable(PortError::NotFound))
                }
            }
            GuestRequest::Call { op, input } => {
                if Some(proof.action.as_str()) != call_action(op) || proof.resource != op {
                    return Err(refused);
                }
                self.calls
                    .lock()
                    .unwrap()
                    .push((proof.subject, op.to_owned()));
                let input: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
                let key = format!("{}:{}", proof.subject, input["key"].as_str().unwrap_or(""));
                let answer = match op {
                    "getJson" => serde_json::json!({
                        "ok": true,
                        "data": self.documents.lock().unwrap().get(&key).cloned().unwrap_or_default(),
                    }),
                    "putJson" => {
                        self.documents
                            .lock()
                            .unwrap()
                            .insert(key, input["data"].clone());
                        serde_json::json!({"ok": true})
                    }
                    _ => serde_json::json!({"ok": true}),
                };
                Ok(answer.to_string().into_bytes())
            }
        }
    }
}

struct World {
    node: Arc<Node>,
    backend: NativeBackend,
    url: String,
    node_subject: Subject,
    _runtime: tokio::runtime::Runtime,
    _state: std::path::PathBuf,
}

fn world() -> World {
    let key = KeyPair::generate().unwrap();
    let certificate = CertificateParams::new(vec!["127.0.0.1".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let url = format!(
        "https://127.0.0.1:{}",
        listener.local_addr().unwrap().port()
    );
    let node = Arc::new(Node::default());
    let served: Arc<dyn GuestApi> = node.clone();
    let chain = vec![certificate.der().clone()];
    let private = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    runtime.spawn(async move {
        serve(listener, chain, private, served, std::future::pending())
            .await
            .unwrap();
    });
    let state = std::env::temp_dir().join(format!("aseman-native-{}", uuid_suffix()));
    let guest = GuestApiClient::new(certificate.pem().as_bytes(), Duration::from_secs(10)).unwrap();
    World {
        node,
        backend: NativeBackend::start(state.clone(), guest, None).unwrap(),
        url,
        node_subject: Subject {
            kind: SubjectKind::Node,
            id: aseman_domain::Uuid::from_bytes([1; 16]),
        },
        _runtime: runtime,
        _state: state,
    }
}

fn uuid_suffix() -> String {
    WorkloadId::new().to_string()
}

impl World {
    /// A javascript workload running `counter.js`, with a registered credential.
    fn workload(&self, machine: &str) -> (WorkloadRecord, Subject) {
        let id = WorkloadId::new();
        let credential =
            WorkloadCredential::generate(id, &self.url, &audience(&self.node_subject)).unwrap();
        self.node.keys.lock().unwrap().insert(
            credential.key_id.clone(),
            PublicKey::decode(&credential.public_key()).unwrap(),
        );
        let mut record = sample_workload("node-a", id, "javascript");
        record.spec.artifact = Artifact {
            kind: ArtifactKind::Blob,
            reference: "programs/counter/module.js".to_owned(),
            digest: digest(COUNTER),
        };
        record.spec.bootstrap.credential = Some(WriteOnlyCredential::new(credential.encode()));
        record.labels.legacy_machine_id = Some(machine.to_owned());
        record.labels.legacy_vm_id = Some(format!("vm-{id}"));
        (record, credential.subject)
    }
}

fn invocation(workload: &WorkloadRecord, by: u32) -> OperationRecord {
    let payload = serde_json::json!({"data": serde_json::json!({"id": "a", "by": by}).to_string()});
    OperationRecord {
        owner: workload.owner.clone(),
        id: OperationId::new(),
        workload_id: Some(workload.id),
        kind: OperationKind::Invoke,
        state: OperationState::Running,
        generation: None,
        request: Some(
            serde_json::json!({"kind": "signal", "key": "tick", "payload": payload}).to_string(),
        ),
        created_at_millis: 0,
        updated_at_millis: 0,
        deadline_millis: None,
        result: None,
        error: None,
    }
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_native_backend_runs_real_programs_as_their_workloads() {
    let world = world();
    let backend = &world.backend;
    let description = backend.describe().unwrap();
    assert_eq!(description.name, "native-legacy");
    let javascript = description
        .runtimes
        .iter()
        .find(|runtime| runtime.runtime == "javascript")
        .unwrap();
    assert!(javascript.invocation && !javascript.exec && !javascript.pause);
    assert_eq!(javascript.deploy.entity_file_name, "module.js");

    // A504 conformance with the real runtime.
    let (conformance, _) = world.workload("11@test");
    check_backend(
        backend,
        conformance,
        r#"{"kind":"signal","key":"tick","payload":{"data":"{}"}}"#,
    );

    // The program runs, and its state calls reach the node as its workload.
    let (workload, subject) = world.workload("12@test");
    let started = backend.step(&workload, ReconcileAction::Start).unwrap();
    assert_eq!(started.state, ObservedWorkloadState::Running);
    backend
        .run(Some(&workload), &invocation(&workload, 2))
        .unwrap();
    let document = format!("{subject}:Json::Counter::a");
    wait_for("the first run", || {
        world
            .node
            .documents
            .lock()
            .unwrap()
            .get(&document)
            .map(|doc| doc["n"].clone())
            == Some(serde_json::json!(2))
    });
    backend
        .run(Some(&workload), &invocation(&workload, 3))
        .unwrap();
    wait_for("the second run", || {
        world
            .node
            .documents
            .lock()
            .unwrap()
            .get(&document)
            .map(|doc| doc["n"].clone())
            == Some(serde_json::json!(5))
    });
    assert!(
        world
            .node
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(caller, _)| caller.kind == SubjectKind::Workload)
    );
    wait_for("the program's log", || {
        backend
            .logs(&workload, 0, 1_000)
            .unwrap()
            .iter()
            .any(|record| record.line.contains("counter a -> 5"))
    });
    assert_eq!(backend.usage(&workload).unwrap().invocations, 2);

    // A credential issued for another workload is refused.
    let (mut foreign, _) = world.workload("13@test");
    let (other, _) = world.workload("14@test");
    foreign.spec.bootstrap.credential = other.spec.bootstrap.credential.clone();
    assert!(matches!(
        backend.step(&foreign, ReconcileAction::Start),
        Err(PortError::Denied(_))
    ));
    // An artifact the node does not have (or that does not match) is never run.
    let (mut missing, _) = world.workload("15@test");
    missing.spec.artifact.digest = format!("sha256:{}", "0".repeat(64));
    assert_eq!(
        backend.step(&missing, ReconcileAction::Start),
        Err(PortError::NotFound)
    );
    // Pausing is refused, never degraded.
    assert_eq!(
        backend.step(&workload, ReconcileAction::Pause),
        Err(PortError::Unsupported("pause"))
    );
    let mut next = workload.clone();
    next.desired.generation = Generation::from_stored(2).unwrap();
    assert_eq!(
        backend.step(&next, ReconcileAction::Delete).unwrap().state,
        ObservedWorkloadState::Stopped
    );
}
