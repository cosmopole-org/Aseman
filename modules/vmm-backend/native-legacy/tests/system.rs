//! P5-05 system test: the node's client, the VMM service over mutual TLS with its
//! PostgreSQL stores and background loop, the native backend as its own process over
//! A504, and a guest API double over TLS, running the real JavaScript runtime.
//! It includes a backend crash: restart reconciliation brings the workload back.
//! Needs `ASEMAN_TEST_POSTGRES_URL`; without it the test is skipped.

use std::collections::BTreeMap;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aseman_application::guest_call::GuestRequest;
use aseman_application::identity::IdentityFailure;
use aseman_contracts::guest_api::{ARTIFACT_ACTION, WorkloadCredential, audience, call_action};
use aseman_contracts::identity::{PublicKey, body_digest, verify_proof_signature};
use aseman_domain::identity::{AuthenticationError, Proof, Subject, SubjectKind};
use aseman_domain::vmm::{
    Artifact, ArtifactKind, DesiredStatus, WorkloadEventType, WriteOnlyCredential,
};
use aseman_domain::{DesiredWorkloadState, Generation, ObservedWorkloadState, WorkloadId};
use aseman_guest_http::server::{GuestApi, serve as serve_guest};
use aseman_ports::conformance::vmm::sample_workload;
use aseman_ports::vmm::{LifecycleCommand, NewWorkload, VmmClient};
use aseman_ports::{ClockPort, PortError};
use aseman_storage_postgres::vmm::PostgresVmmStore;
use aseman_vmm_backend_grpc::client::GrpcBackend;
use aseman_vmm_http::client::{ClientTls, HttpVmmClient};
use aseman_vmm_http::server::{ServerTls, VmmHttpState, serve};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

const COUNTER: &[u8] = include_bytes!("../../../../modules/runtime/javascript/examples/counter.js");
const BACKEND: &str = env!("CARGO_BIN_EXE_aseman-vmm-backend-native");

fn digest(bytes: &[u8]) -> String {
    let hash: [u8; 32] = Sha256::digest(bytes).into();
    format!(
        "sha256:{}",
        hash.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

#[derive(Default)]
struct Node {
    keys: Mutex<BTreeMap<String, PublicKey>>,
    documents: Mutex<BTreeMap<String, serde_json::Value>>,
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
                if proof.action != ARTIFACT_ACTION || wanted != digest(COUNTER) {
                    return Err(refused);
                }
                Ok(COUNTER.to_vec())
            }
            GuestRequest::Call { op, input } => {
                if Some(proof.action.as_str()) != call_action(op) {
                    return Err(refused);
                }
                let input: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
                let key = format!("{}:{}", proof.subject, input["key"].as_str().unwrap_or(""));
                Ok(match op {
                    "getJson" => serde_json::json!({
                        "ok": true,
                        "data": self.documents.lock().unwrap().get(&key).cloned().unwrap_or_default(),
                    }),
                    "putJson" => {
                        self.documents.lock().unwrap().insert(key, input["data"].clone());
                        serde_json::json!({"ok": true})
                    }
                    _ => serde_json::json!({"ok": true}),
                }
                .to_string()
                .into_bytes())
            }
        }
    }
}

struct SystemClock;

impl ClockPort for SystemClock {
    fn unix_millis(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(0))
    }
}

struct Leaf {
    der: CertificateDer<'static>,
    pem: String,
    key_pem: String,
    key_der: Vec<u8>,
}

fn leaf(issuer: &CertifiedIssuer<'static, KeyPair>, names: &[&str]) -> Leaf {
    let key = KeyPair::generate().unwrap();
    let certificate = CertificateParams::new(
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
    )
    .unwrap()
    .signed_by(&key, issuer)
    .unwrap();
    Leaf {
        der: certificate.der().clone(),
        pem: certificate.pem(),
        key_pem: key.serialize_pem(),
        key_der: key.serialize_der(),
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn spawn_backend(port: u16, config: &std::path::Path) -> Child {
    Command::new(BACKEND)
        .arg(format!("127.0.0.1:{port}"))
        .arg(config)
        .spawn()
        .unwrap()
}

fn wait_for<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(value) = ready() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn the_node_drives_the_native_backend_through_the_vmm_service() {
    let Some(url) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping the VMM system test");
        return;
    };
    let scratch = std::env::temp_dir().join(format!("aseman-vmm-system-{}", WorkloadId::new()));
    std::fs::create_dir_all(&scratch).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    // The node's guest API (a double with real proof checks).
    let authority = {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap()
    };
    let ca_path = scratch.join("ca.pem");
    std::fs::write(&ca_path, authority.pem()).unwrap();
    let guest_certificate = leaf(&authority, &["127.0.0.1"]);
    let guest_listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let guest_url = format!(
        "https://127.0.0.1:{}",
        guest_listener.local_addr().unwrap().port()
    );
    let node = Arc::new(Node::default());
    let guest_api: Arc<dyn GuestApi> = node.clone();
    let guest_chain = vec![guest_certificate.der.clone()];
    let guest_key =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(guest_certificate.key_der.clone()));
    runtime.spawn(async move {
        serve_guest(
            guest_listener,
            guest_chain,
            guest_key,
            guest_api,
            std::future::pending(),
        )
        .await
        .unwrap();
    });

    // The native backend, as its own process.
    let backend_port = free_port();
    let backend_config = scratch.join("backend.json");
    std::fs::write(
        &backend_config,
        serde_json::json!({
            "state_dir": scratch.join("backend"),
            "node_ca": ca_path,
        })
        .to_string(),
    )
    .unwrap();
    let mut backend_process = spawn_backend(backend_port, &backend_config);
    let backend = Arc::new(
        GrpcBackend::connect(
            &format!("http://127.0.0.1:{backend_port}"),
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    wait_for("the backend", || {
        aseman_ports::vmm::VmmBackend::describe(&*backend).ok()
    });

    // The VMM service on a fresh database.
    let database = format!("aseman_vmm_system_{}", WorkloadId::new().as_uuid().simple());
    let mut admin = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut database_url: postgres::Config = url.parse().unwrap();
    database_url.dbname(&database);
    let store = Arc::new(PostgresVmmStore::connect_config(database_url, 4).unwrap());
    store.migrate().unwrap();
    let state = Arc::new(VmmHttpState {
        workloads: store.clone(),
        operations: store.clone(),
        events: store.clone(),
        idempotency: store,
        backend: backend.clone(),
        clock: Arc::new(SystemClock),
        max_request_bytes: 1024 * 1024,
    });
    let server = leaf(&authority, &["127.0.0.1"]);
    let client_leaf = leaf(&authority, &["node-1"]);
    let tls = ServerTls {
        certificate_chain: vec![server.der.clone()],
        private_key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server.key_der.clone())),
        client_roots: vec![authority.der().clone()],
        clients: BTreeMap::from([(
            Sha256::digest(client_leaf.der.as_ref()).into(),
            "node-1".to_owned(),
        )]),
    };
    let vmm_listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let vmm_url = format!(
        "https://127.0.0.1:{}",
        vmm_listener.local_addr().unwrap().port()
    );
    let served = state.clone();
    runtime.spawn(async move {
        serve(vmm_listener, tls, served, std::future::pending())
            .await
            .unwrap();
    });
    let running = Arc::new(AtomicBool::new(true));
    let loop_state = state.clone();
    let loop_running = running.clone();
    let worker = std::thread::spawn(move || {
        while loop_running.load(Ordering::Relaxed) {
            let service = loop_state.service();
            let _ = service.execute_pending(100);
            let _ = service.observe();
            let _ = service.reconcile();
            std::thread::sleep(Duration::from_millis(100));
        }
    });

    // The node.
    let client = HttpVmmClient::new(
        &vmm_url,
        &ClientTls {
            server_roots_pem: authority.pem().into_bytes(),
            identity_pem: format!("{}{}", client_leaf.pem, client_leaf.key_pem).into_bytes(),
        },
        "node-1",
        Duration::from_secs(30),
    )
    .unwrap();
    let node_subject = Subject {
        kind: SubjectKind::Node,
        id: aseman_domain::Uuid::from_bytes([1; 16]),
    };
    let id = WorkloadId::new();
    let credential =
        WorkloadCredential::generate(id, &guest_url, &audience(&node_subject)).unwrap();
    node.keys.lock().unwrap().insert(
        credential.key_id.clone(),
        PublicKey::decode(&credential.public_key()).unwrap(),
    );
    let mut record = sample_workload("", id, "javascript");
    record.spec.artifact = Artifact {
        kind: ArtifactKind::Blob,
        reference: "programs/counter/module.js".to_owned(),
        digest: digest(COUNTER),
    };
    record.spec.bootstrap.credential = Some(WriteOnlyCredential::new(credential.encode()));
    record.labels.legacy_machine_id = Some("21@system".to_owned());
    record.labels.legacy_vm_id = Some("signal".to_owned());
    client
        .create(
            &NewWorkload {
                id,
                labels: record.labels.clone(),
                spec: record.spec.clone(),
                desired: DesiredStatus {
                    state: DesiredWorkloadState::Running,
                    generation: Generation::INITIAL,
                },
            },
            &format!("create-{}", id.as_uuid().simple()),
        )
        .unwrap();
    let observed_state = |client: &HttpVmmClient| {
        client
            .workload(id)
            .ok()
            .flatten()
            .and_then(|workload| workload.observed.map(|observed| observed.state))
    };
    wait_for("the workload to run", || {
        (observed_state(&client) == Some(ObservedWorkloadState::Running)).then_some(())
    });

    let document = format!("{}:Json::Counter::a", credential.subject);
    let invoke = |by: u32, n: u64| {
        let payload =
            serde_json::json!({"data": serde_json::json!({"id": "a", "by": by}).to_string()});
        client
            .invoke(
                id,
                &serde_json::json!({"kind": "signal", "key": "tick", "payload": payload})
                    .to_string(),
                &format!("invoke-{}", WorkloadId::new().as_uuid().simple()),
            )
            .unwrap();
        wait_for("the counter", || {
            (node
                .documents
                .lock()
                .unwrap()
                .get(&document)
                .map(|doc| doc["n"].clone())
                == Some(serde_json::json!(n)))
            .then_some(())
        });
    };
    invoke(2, 2);

    // Logs over A501 (SSE).
    let raw = reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .tls_built_in_root_certs(false)
        .add_root_certificate(reqwest::Certificate::from_pem(authority.pem().as_bytes()).unwrap())
        .identity(
            reqwest::Identity::from_pem(
                format!("{}{}", client_leaf.pem, client_leaf.key_pem).as_bytes(),
            )
            .unwrap(),
        )
        .build()
        .unwrap();
    wait_for("the program's log line", || {
        let body = raw
            .get(format!("{vmm_url}/v1/workloads/{id}/logs?follow=false"))
            .send()
            .ok()?
            .text()
            .ok()?;
        body.contains("counter a -> 2").then_some(())
    });

    // The backend crashes: the instance is lost, reconciliation restarts it, and it
    // serves again.
    backend_process.kill().unwrap();
    backend_process.wait().unwrap();
    let mut backend_process = spawn_backend(backend_port, &backend_config);
    wait_for("the workload to be lost", || {
        (observed_state(&client) == Some(ObservedWorkloadState::Lost)).then_some(())
    });
    wait_for("the workload to be restarted", || {
        (observed_state(&client) == Some(ObservedWorkloadState::Running)).then_some(())
    });
    invoke(3, 5);

    // Deletion converges and is announced.
    client
        .command(
            id,
            LifecycleCommand::Delete,
            Generation::from_stored(2).unwrap(),
            &format!("delete-{}", id.as_uuid().simple()),
        )
        .unwrap();
    wait_for("the deletion", || {
        client
            .events_after(0, 10_000)
            .ok()?
            .events
            .iter()
            .any(|event| event.workload_id == id && event.event_type == WorkloadEventType::Deleted)
            .then_some(())
    });
    assert_eq!(
        client.invoke(
            id,
            r#"{"kind":"signal","key":"tick","payload":{}}"#,
            "invoke-after-delete-0001"
        ),
        Err(PortError::Failed(
            "workload_deleted: the workload is deleted".to_owned()
        ))
    );

    running.store(false, Ordering::Relaxed);
    worker.join().unwrap();
    backend_process.kill().unwrap();
    let _ = backend_process.wait();
    // The servers' tasks go first; the store's last reference (and its synchronous
    // PostgreSQL connections) is released here, outside the runtime.
    drop(runtime);
    drop(state);
    admin
        .batch_execute(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .unwrap();
    let _ = std::fs::remove_dir_all(&scratch);
}
