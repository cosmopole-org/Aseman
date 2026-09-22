//! End to end over real mutual TLS: the client against the server, with the memory
//! reference stores and the scripted backend.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use aseman_application::vmm::VmmService;
use aseman_domain::vmm::{
    DeployConventions, DesiredStatus, RuntimeCapabilities, WorkloadEventType, WriteOnlyCredential,
};
use aseman_domain::{DesiredWorkloadState, ObservedWorkloadState, OperationState};
use aseman_ports::ClockPort;
use aseman_ports::conformance::vmm::{MemoryVmmStores, ScriptedBackend, sample_workload};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

use super::*;
use crate::server::{ServerTls, VmmHttpState, serve};

struct Clock(AtomicI64);

impl ClockPort for Clock {
    fn unix_millis(&self) -> i64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

struct Leaf {
    der: CertificateDer<'static>,
    pem: String,
    key_pem: String,
    key_der: Vec<u8>,
}

fn ca() -> CertifiedIssuer<'static, KeyPair> {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap()
}

fn leaf(issuer: &CertifiedIssuer<'static, KeyPair>, names: &[&str]) -> Leaf {
    let key = KeyPair::generate().unwrap();
    let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    let certificate = CertificateParams::new(names)
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

struct Harness {
    stores: Arc<MemoryVmmStores>,
    backend: Arc<ScriptedBackend>,
    clock: Arc<Clock>,
    base: String,
    ca_pem: String,
    node_a: Leaf,
    node_b: Leaf,
    stranger: Leaf,
    _runtime: tokio::runtime::Runtime,
}

impl Harness {
    fn start() -> Self {
        let authority = ca();
        let server = leaf(&authority, &["127.0.0.1", "localhost"]);
        let node_a = leaf(&authority, &["node-a"]);
        let node_b = leaf(&authority, &["node-b"]);
        let stranger = leaf(&authority, &["stranger"]);
        let fingerprint = |leaf: &Leaf| -> [u8; 32] { Sha256::digest(leaf.der.as_ref()).into() };
        let tls = ServerTls {
            certificate_chain: vec![server.der.clone()],
            private_key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server.key_der.clone())),
            client_roots: vec![authority.der().clone()],
            clients: BTreeMap::from([
                (fingerprint(&node_a), "node-a".to_owned()),
                (fingerprint(&node_b), "node-b".to_owned()),
            ]),
        };
        let stores = Arc::new(MemoryVmmStores::default());
        let backend = Arc::new(ScriptedBackend::new(vec![
            RuntimeCapabilities {
                runtime: "wasm".to_owned(),
                invocation: true,
                deploy: DeployConventions {
                    entity_file_name: "module.wasm".to_owned(),
                    ..DeployConventions::default()
                },
                ..RuntimeCapabilities::default()
            },
            RuntimeCapabilities {
                runtime: "docker".to_owned(),
                invocation: true,
                long_running: true,
                http_ingress: true,
                files: true,
                ..RuntimeCapabilities::default()
            },
        ]));
        let clock = Arc::new(Clock(AtomicI64::new(1_000)));
        let state = Arc::new(VmmHttpState {
            workloads: stores.clone(),
            operations: stores.clone(),
            events: stores.clone(),
            idempotency: stores.clone(),
            backend: backend.clone(),
            clock: clock.clone(),
            max_request_bytes: 64 * 1024,
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let address = listener.local_addr().unwrap();
        runtime.spawn(async move {
            serve(listener, tls, state, std::future::pending())
                .await
                .unwrap();
        });
        Self {
            stores,
            backend,
            clock,
            base: format!("https://127.0.0.1:{}", address.port()),
            ca_pem: authority.pem(),
            node_a,
            node_b,
            stranger,
            _runtime: runtime,
        }
    }

    fn client(&self, leaf: &Leaf, owner: &str) -> HttpVmmClient {
        HttpVmmClient::new(
            &self.base,
            &ClientTls {
                server_roots_pem: self.ca_pem.clone().into_bytes(),
                identity_pem: format!("{}{}", leaf.pem, leaf.key_pem).into_bytes(),
            },
            owner,
            Duration::from_secs(10),
        )
        .unwrap()
    }

    fn drain(&self) {
        let service = VmmService {
            workloads: &*self.stores,
            operations: &*self.stores,
            events: &*self.stores,
            backend: &*self.backend,
            clock: &*self.clock,
        };
        for _ in 0..10 {
            service.execute_pending(100).unwrap();
        }
    }
}

fn new_workload(runtime: &str) -> NewWorkload {
    let mut record = sample_workload("", WorkloadId::new(), runtime);
    record.spec.bootstrap.credential =
        Some(WriteOnlyCredential::new("bootstrap-secret".to_owned()));
    NewWorkload {
        id: record.id,
        labels: record.labels,
        spec: record.spec,
        desired: DesiredStatus {
            state: DesiredWorkloadState::Running,
            generation: Generation::INITIAL,
        },
    }
}

fn generation(value: u64) -> Generation {
    Generation::from_stored(value).unwrap()
}

#[test]
fn the_node_client_drives_the_vmm_over_mutual_tls() {
    let harness = Harness::start();
    let client = harness.client(&harness.node_a, "node-a");
    let capabilities = client.capabilities().unwrap();
    assert_eq!(capabilities.name, "scripted");
    assert_eq!(capabilities.contract, aseman_contracts::vmm::API_VERSION);
    assert_eq!(capabilities.runtimes.len(), 2);

    let workload = new_workload("docker");
    let created = client.create(&workload, "create-0000000000001").unwrap();
    // A retry under the same key returns the same operation; another body under it
    // is refused.
    assert_eq!(
        client.create(&workload, "create-0000000000001").unwrap().id,
        created.id
    );
    let mut other = workload.clone();
    other.id = WorkloadId::new();
    assert!(matches!(
        client.create(&other, "create-0000000000001"),
        Err(PortError::Failed(message)) if message.starts_with("idempotency_key_reused")
    ));
    assert_eq!(
        client
            .create(&workload, "create-0000000000002")
            .unwrap_err(),
        PortError::Conflict
    );
    harness.drain();
    assert_eq!(
        client.operation(created.id).unwrap().unwrap().state,
        OperationState::Succeeded
    );
    let record = client.workload(workload.id).unwrap().unwrap();
    assert_eq!(record.owner, "node-a");
    assert_eq!(
        record.observed.as_ref().map(|observed| observed.state),
        Some(ObservedWorkloadState::Running)
    );
    // The credential is write-only.
    assert_eq!(record.spec.bootstrap.credential, None);

    let stop = client
        .command(
            workload.id,
            LifecycleCommand::Stop,
            generation(2),
            "stop-00000000000002",
        )
        .unwrap();
    assert_eq!(stop.generation, Some(generation(2)));
    assert_eq!(
        client
            .command(
                workload.id,
                LifecycleCommand::Start,
                generation(1),
                "start-0000000000001"
            )
            .unwrap_err(),
        PortError::Conflict
    );
    harness.drain();
    let invoke = client
        .command(
            workload.id,
            LifecycleCommand::Start,
            generation(3),
            "start-0000000000003",
        )
        .unwrap();
    harness.drain();
    assert_eq!(
        client.operation(invoke.id).unwrap().unwrap().state,
        OperationState::Succeeded
    );
    let invocation = client
        .invoke(
            workload.id,
            r#"{"kind":"signal","key":"tick","payload":{"n":1}}"#,
            "invoke-000000000001",
        )
        .unwrap();
    harness.drain();
    let done = client.operation(invocation.id).unwrap().unwrap();
    assert_eq!(done.state, OperationState::Succeeded);
    assert!(done.result.unwrap().contains("tick"));
    let answer = client
        .forward_http(
            workload.id,
            r#"{"method":"GET","path":"/","headers":{}}"#,
            "http-00000000000001",
        )
        .unwrap();
    assert!(answer.contains("\"status\":200"));

    let events = client.events_after(0, 1_000).unwrap();
    assert!(!events.resync);
    assert!(
        events
            .events
            .iter()
            .any(|event| event.event_type == WorkloadEventType::Observed)
    );
    let last = events.events.last().unwrap().sequence;
    assert!(client.events_after(last, 1_000).unwrap().events.is_empty());

    // Another node sees nothing of node-a's.
    let other_node = harness.client(&harness.node_b, "node-b");
    assert_eq!(other_node.workload(workload.id).unwrap(), None);
    assert!(other_node.events_after(0, 1_000).unwrap().events.is_empty());
    assert_eq!(
        other_node
            .command(
                workload.id,
                LifecycleCommand::Stop,
                generation(9),
                "stop-00000000000009"
            )
            .unwrap_err(),
        PortError::NotFound
    );
}

#[test]
fn unsupported_and_unauthenticated_requests_are_refused() {
    let harness = Harness::start();
    let client = harness.client(&harness.node_a, "node-a");
    let workload = new_workload("wasm");
    client.create(&workload, "create-wasm-00000001").unwrap();
    harness.drain();
    assert_eq!(
        client
            .forward_http(
                workload.id,
                r#"{"method":"GET","path":"/","headers":{}}"#,
                "http-wasm-000000001"
            )
            .unwrap_err(),
        PortError::Unsupported("unsupported_operation")
    );
    // A certificate from the right authority that is not admitted gets no answer.
    let stranger = harness.client(&harness.stranger, "stranger");
    assert_eq!(
        stranger.capabilities().unwrap_err(),
        PortError::Unavailable("VMM")
    );
    // No client certificate at all.
    let anonymous = reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .tls_built_in_root_certs(false)
        .add_root_certificate(reqwest::Certificate::from_pem(harness.ca_pem.as_bytes()).unwrap())
        .build()
        .unwrap();
    assert!(
        anonymous
            .get(format!("{}/v1/capabilities", harness.base))
            .send()
            .is_err()
    );
    // A mutation without an idempotency key is a problem, with the request ID echoed.
    let raw = reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .tls_built_in_root_certs(false)
        .add_root_certificate(reqwest::Certificate::from_pem(harness.ca_pem.as_bytes()).unwrap())
        .identity(
            reqwest::Identity::from_pem(
                format!("{}{}", harness.node_a.pem, harness.node_a.key_pem).as_bytes(),
            )
            .unwrap(),
        )
        .build()
        .unwrap();
    let response = raw
        .post(format!(
            "{}/v1/workloads/{}/stop",
            harness.base, workload.id
        ))
        .header("X-Request-Id", "trace-me")
        .json(&serde_json::json!({"generation": 2}))
        .send()
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(response.headers()["x-request-id"], "trace-me");
    assert_eq!(
        response.headers()["content-type"],
        "application/problem+json"
    );
    let problem: Problem = response.json().unwrap();
    assert_eq!(problem.code, ProblemCode::InvalidRequest);
    assert_eq!(problem.request_id, "trace-me");
    // An oversized body is a problem too.
    let response = raw
        .post(format!(
            "{}/v1/workloads/{}/exec",
            harness.base, workload.id
        ))
        .header(IDEMPOTENCY_KEY, "big-body-0000000001")
        .body(vec![b'x'; 128 * 1024])
        .send()
        .unwrap();
    assert_eq!(response.status(), 413);
    // A stale generation names the current one.
    let response = raw
        .post(format!(
            "{}/v1/workloads/{}/stop",
            harness.base, workload.id
        ))
        .header(IDEMPOTENCY_KEY, "stale-gen-000000001")
        .json(&serde_json::json!({"generation": 0}))
        .send()
        .unwrap();
    assert_eq!(response.status(), 400, "generation 0 is invalid");
    let workload_response = raw
        .get(format!("{}/v1/workloads/{}", harness.base, workload.id))
        .send()
        .unwrap();
    assert_eq!(workload_response.headers()["etag"], "\"2\"");
}
