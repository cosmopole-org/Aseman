//! A505 parity for the docker runtime on the real Docker daemon: the native backend
//! builds the entity's Dockerfile (fetched and digest-checked through the guest API),
//! runs the container, forwards HTTP into it, and stops and deletes it. Skipped when
//! Docker is not reachable.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aseman_application::guest_call::GuestRequest;
use aseman_application::identity::IdentityFailure;
use aseman_contracts::guest_api::{ARTIFACT_ACTION, WorkloadCredential, audience};
use aseman_contracts::identity::{PublicKey, body_digest, verify_proof_signature};
use aseman_contracts::vmm::{HttpRequest, HttpResponse};
use aseman_domain::identity::{AuthenticationError, Proof, Subject, SubjectKind};
use aseman_domain::vmm::{Artifact, ArtifactKind, ReconcileAction, WriteOnlyCredential};
use aseman_domain::{Generation, ObservedWorkloadState, WorkloadId};
use aseman_guest_http::client::GuestApiClient;
use aseman_guest_http::server::{GuestApi, serve};
use aseman_ports::conformance::vmm::sample_workload;
use aseman_ports::vmm::VmmBackend;
use aseman_vmm_backend_native::backend::NativeBackend;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rcgen::{CertificateParams, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

struct Node {
    key: Mutex<Option<PublicKey>>,
    dockerfile: Vec<u8>,
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
            .key
            .lock()
            .unwrap()
            .clone()
            .ok_or(AuthenticationError::UnknownKey)?;
        verify_proof_signature(proof, &key)?;
        if proof.body_digest != body_digest(body) {
            return Err(AuthenticationError::BodyDigestMismatch.into());
        }
        match request {
            GuestRequest::Artifact { digest: wanted }
                if proof.action == ARTIFACT_ACTION && wanted == digest(&self.dockerfile) =>
            {
                Ok(self.dockerfile.clone())
            }
            GuestRequest::Artifact { .. } => Err(IdentityFailure::Refused("not this workload's")),
            GuestRequest::Call { .. } => Ok(b"{\"ok\":true}".to_vec()),
        }
    }
}

fn docker_available() -> bool {
    std::process::Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn container_exists(name: &str) -> bool {
    std::process::Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("name=^/{name}$"),
            "--format",
            "{{.Names}}",
        ])
        .output()
        .is_ok_and(|output| !String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

#[test]
fn docker_workloads_build_serve_http_and_go_away() {
    if !docker_available() {
        eprintln!("Docker is not reachable; skipping the docker parity test");
        return;
    }
    let port = aseman_config::runtime_config().vm_http_port;
    let dockerfile = format!(
        "FROM caddy:2-alpine\nRUN mkdir -p /srv /app/input && printf 'hello from docker' > /srv/index.html && chown -R 1000:1000 /app\nWORKDIR /app\n# Runs as the backend's user: an unprivileged backend cannot purge a sandbox a\n# root container wrote into (LD-28).\nUSER 1000:1000\nENV XDG_DATA_HOME=/app/input XDG_CONFIG_HOME=/app/input\nCMD [\"caddy\", \"file-server\", \"--listen\", \":{port}\", \"--root\", \"/srv\"]\n"
    )
    .into_bytes();
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
    let node = Arc::new(Node {
        key: Mutex::new(None),
        dockerfile: dockerfile.clone(),
    });
    let served: Arc<dyn GuestApi> = node.clone();
    let chain = vec![certificate.der().clone()];
    let private = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    runtime.spawn(async move {
        serve(listener, chain, private, served, std::future::pending())
            .await
            .unwrap()
    });
    let state = std::env::temp_dir().join(format!("aseman-docker-{}", WorkloadId::new()));
    let backend = NativeBackend::start(
        state.clone(),
        GuestApiClient::new(certificate.pem().as_bytes(), Duration::from_secs(30)).unwrap(),
        None,
    )
    .unwrap();

    let id = WorkloadId::new();
    let node_subject = Subject {
        kind: SubjectKind::Node,
        id: aseman_domain::Uuid::from_bytes([1; 16]),
    };
    let credential = WorkloadCredential::generate(id, &url, &audience(&node_subject)).unwrap();
    *node.key.lock().unwrap() = Some(PublicKey::decode(&credential.public_key()).unwrap());
    let machine = format!("p{}@parity", &id.as_uuid().simple().to_string()[..8]);
    let mut workload = sample_workload("node-a", id, "docker");
    workload.spec.artifact = Artifact {
        kind: ArtifactKind::Blob,
        reference: "machines/parity/entities/web/Dockerfile".to_owned(),
        digest: digest(&dockerfile),
    };
    workload.spec.bootstrap.credential = Some(WriteOnlyCredential::new(credential.encode()));
    workload.labels.entity_id = "web".to_owned();
    workload.labels.legacy_machine_id = Some(machine.clone());
    workload.labels.legacy_vm_id = Some("vm1".to_owned());
    let container = format!("{}_vm1", machine.replace('@', "_"));

    let started = backend.step(&workload, ReconcileAction::Start).unwrap();
    assert_eq!(
        started.state,
        ObservedWorkloadState::Running,
        "{:?}",
        started.reason
    );
    assert!(
        container_exists(&container),
        "the container {container} runs"
    );
    // The image build's output is this workload's `build` stream, not a node-wide
    // one nobody owns (LD-30).
    let build: Vec<_> = backend
        .logs(&workload, 0, 1000)
        .unwrap()
        .into_iter()
        .filter(|record| record.stream == aseman_domain::vmm::LogStream::Build)
        .collect();
    assert!(
        build.iter().any(|record| record.line.contains("Step ")),
        "the build log is the workload's: {build:?}"
    );

    // HTTP reaches the server inside the container.
    let request = HttpRequest {
        method: "GET".to_owned(),
        path: "/".to_owned(),
        query: None,
        headers: BTreeMap::new(),
        body: None,
        port: None,
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let body = loop {
        if let Ok(answer) =
            backend.forward_http(&workload, &serde_json::to_string(&request).unwrap())
        {
            let response: HttpResponse = serde_json::from_str(&answer).unwrap();
            if response.status == 200 {
                break URL_SAFE_NO_PAD
                    .decode(response.body.unwrap_or_default())
                    .unwrap();
            }
        }
        assert!(Instant::now() < deadline, "the container never served HTTP");
        std::thread::sleep(Duration::from_millis(300));
    };
    assert_eq!(body, b"hello from docker");

    // Files go in, and commands run inside the container.
    backend
        .put_file(&workload, "app/input/note.txt", b"written by the node")
        .unwrap();
    let exec = aseman_domain::vmm::OperationRecord {
        owner: "node-a".to_owned(),
        id: aseman_domain::OperationId::new(),
        workload_id: Some(workload.id),
        kind: aseman_domain::vmm::OperationKind::Exec,
        state: aseman_domain::OperationState::Running,
        generation: None,
        request: Some(
            serde_json::json!({"command": ["cat", "/app/input/note.txt", "/srv/index.html"]})
                .to_string(),
        ),
        created_at_millis: 0,
        updated_at_millis: 0,
        deadline_millis: None,
        result: None,
        error: None,
    };
    let result: aseman_contracts::vmm::ExecResult =
        serde_json::from_str(&backend.run(Some(&workload), &exec).unwrap()).unwrap();
    let stdout = String::from_utf8(URL_SAFE_NO_PAD.decode(result.stdout).unwrap()).unwrap();
    assert!(stdout.contains("written by the node"), "{stdout}");
    assert!(stdout.contains("hello from docker"), "{stdout}");
    assert!(matches!(
        backend.put_file(&workload, "app/input/blob.bin", &[0xff, 0xfe]),
        Err(aseman_ports::PortError::Unsupported(_))
    ));

    // A docker entity publishes no endpoints of its own (the ingress fronts it).
    assert!(backend.endpoints(&workload).unwrap().is_empty());
    assert!(matches!(
        backend.get_file(&workload, "srv/index.html"),
        Err(aseman_ports::PortError::NotFound)
    ));

    let mut next = workload.clone();
    next.desired.generation = Generation::from_stored(2).unwrap();
    assert_eq!(
        backend.step(&next, ReconcileAction::Stop).unwrap().state,
        ObservedWorkloadState::Stopped
    );
    next.desired.generation = Generation::from_stored(3).unwrap();
    backend.step(&next, ReconcileAction::Delete).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while container_exists(&container) {
        assert!(
            Instant::now() < deadline,
            "the container {container} was not removed"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = std::process::Command::new("docker")
        .args([
            "image",
            "rm",
            "-f",
            &format!("{}/web", machine.replace('@', "_")),
        ])
        .output();
    let _ = std::fs::remove_dir_all(state);
}
