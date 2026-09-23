//! The Nomad backend against a real Nomad cluster with the Docker driver: the A504
//! conformance kit plus the mapping's own promises — bridge networking, HTTP reaching
//! the allocation, logs, usage, and a purge that leaves nothing behind.
//!
//! Skipped when no Nomad cluster answers at `ASEMAN_NOMAD_ENDPOINT` (or the default
//! `http://127.0.0.1:4646`), so the suite stays green where a scheduler is not
//! installed. Aseman never installs one (ADR 0002).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aseman_contracts::vmm::{HttpRequest, HttpResponse};
use aseman_domain::vmm::{Artifact, ArtifactKind, IngressPort, PortProtocol, ReconcileAction};
use aseman_domain::{ObservedWorkloadState, WorkloadId};
use aseman_ports::conformance::vmm::sample_workload;
use aseman_ports::vmm::VmmBackend;
use aseman_vmm_backend_conformance::check_backend;
use aseman_vmm_backend_nomad::backend::{NomadBackend, Runtime, runtime_capabilities};
use aseman_vmm_backend_nomad::client::Nomad;
use aseman_vmm_backend_nomad::job::{Execution, NetworkMode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The image the test workload runs: a busybox HTTP server whose page is the
/// greeting the workload's environment carries.
const IMAGE: &str = "aseman-nomad-test:1";

const DOCKERFILE: &str = "FROM busybox:1.36\n\
     CMD sh -c 'mkdir -p /www; echo \"$ASEMAN_TEST_GREETING\" > /www/index.html; \
     echo serving; exec httpd -f -p 8080 -h /www'\n";

fn endpoint() -> String {
    aseman_config::IntegrationTestConfig::from_process()
        .nomad_endpoint
        .unwrap_or_else(|| "http://127.0.0.1:4646".to_owned())
}

/// Build the test image on the worker's Docker daemon. Nomad's Docker driver uses a
/// local image when it has one, so nothing is pushed anywhere.
/// Build the image at most once per test binary: the tests run in parallel and two
/// concurrent `docker build` calls for the same tag race each other.
fn build_image() -> bool {
    static BUILT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *BUILT.get_or_init(build_image_once)
}

fn build_image_once() -> bool {
    let mut child = match std::process::Command::new("docker")
        .args(["build", "-q", "-t", IMAGE, "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        if stdin.write_all(DOCKERFILE.as_bytes()).is_err() {
            return false;
        }
    }
    child.wait().is_ok_and(|status| status.success())
}

/// The CNI network the operator configured for Aseman workloads: it denies egress by
/// default, which is what a workload's policy asks for (A406).
fn restricted() -> NetworkMode {
    NetworkMode::Cni {
        name: "aseman-restricted".to_owned(),
        denies_egress: true,
    }
}

fn cluster() -> Option<Nomad> {
    let nomad = Nomad::new(endpoint(), "default", None, Duration::from_secs(20)).ok()?;
    nomad.agent_version().ok().map(|_| nomad)
}

fn backend(nomad: Nomad) -> NomadBackend {
    NomadBackend::start(
        nomad,
        vec![Runtime {
            capabilities: runtime_capabilities("docker", &Execution::Image)
                .expect("the docker runtime is in the A505 matrix"),
            execution: Execution::Image,
        }],
        vec!["dc1".to_owned()],
        restricted(),
    )
    .expect("the cluster answers")
}

/// Wait until `check` produces a value. A check that keeps failing fails the test
/// with the last refusal, so a broken backend is never mistaken for a slow scheduler.
fn until(
    what: &str,
    deadline: Duration,
    mut check: impl FnMut() -> Result<Option<String>, aseman_ports::PortError>,
) -> String {
    let end = Instant::now() + deadline;
    let mut last = String::new();
    loop {
        match check() {
            Ok(Some(value)) => return value,
            Ok(None) => {}
            Err(error) => last = format!("; last refusal: {error:?}"),
        }
        assert!(Instant::now() < end, "never saw {what}{last}");
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
fn a_workload_runs_serves_http_and_is_purged() {
    let Some(nomad) = cluster() else {
        eprintln!("no Nomad cluster at {}; skipped", endpoint());
        return;
    };
    assert!(build_image(), "the test image builds on this worker");
    let backend = backend(nomad);

    let mut workload = sample_workload("node:test", WorkloadId::new(), "docker");
    workload.spec.artifact = Artifact {
        kind: ArtifactKind::Oci,
        reference: IMAGE.to_owned(),
        digest: format!("sha256:{}", "0".repeat(64)),
    };
    workload.spec.network.ingress = vec![IngressPort {
        name: "http".to_owned(),
        port: 8080,
        protocol: PortProtocol::Http,
    }];
    workload.spec.environment = BTreeMap::from([(
        "ASEMAN_TEST_GREETING".to_owned(),
        "hello from nomad".to_owned(),
    )]);

    let started = backend
        .step(&workload, ReconcileAction::Start)
        .expect("start");
    assert_eq!(started.generation, workload.desired.generation);

    // Nomad places the allocation; the workload is then observed running.
    until("the allocation to run", Duration::from_secs(120), || {
        let observation = backend.step(&workload, ReconcileAction::None)?;
        Ok((observation.state == ObservedWorkloadState::Running)
            .then(|| format!("{:?}", observation.state)))
    });

    // The observation is for the generation the job carries, from observe_all too.
    let all = backend.observe_all().expect("observe_all");
    let (_, observed) = all
        .iter()
        .find(|(id, _)| *id == workload.id)
        .expect("the workload is observed");
    assert_eq!(observed.generation, workload.desired.generation);

    // The endpoint is the allocation's bridge address, and HTTP reaches it.
    let endpoints = until("an endpoint", Duration::from_secs(60), || {
        Ok(backend
            .endpoints(&workload)?
            .into_iter()
            .next()
            .map(|found| format!("{found:?}")))
    });
    assert!(endpoints.contains("http"), "{endpoints}");

    let request = HttpRequest {
        method: "GET".to_owned(),
        path: "/index.html".to_owned(),
        query: None,
        headers: BTreeMap::new(),
        body: None,
        port: None,
    };
    let request = serde_json::to_string(&request).expect("the request is JSON");
    let body = until(
        "the server inside the allocation",
        Duration::from_secs(90),
        || {
            let answer = backend.forward_http(&workload, &request)?;
            let response: HttpResponse = serde_json::from_str(&answer).expect("an A501 response");
            Ok((response.status == 200).then(|| {
                let bytes = URL_SAFE_NO_PAD
                    .decode(response.body.unwrap_or_default())
                    .expect("the body is base64url");
                String::from_utf8(bytes).expect("the page is text")
            }))
        },
    );
    assert!(body.contains("hello from nomad"), "{body}");

    // Logs come from the allocation, in order, and `after` is honored.
    let logs = until("log lines", Duration::from_secs(60), || {
        let records = backend.logs(&workload, 0, 100)?;
        Ok((!records.is_empty()).then(|| format!("{records:?}")))
    });
    assert!(logs.contains("serving"), "{logs}");
    let records = backend.logs(&workload, 0, 100).expect("logs");
    assert!(
        records
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence),
        "log sequences increase"
    );
    let first = records.first().expect("a record").sequence;
    assert!(
        backend
            .logs(&workload, first, 100)
            .expect("logs after")
            .iter()
            .all(|record| record.sequence > first)
    );

    // Usage is cumulative and sequenced.
    let first_usage = backend.usage(&workload).expect("usage");
    let second_usage = backend.usage(&workload).expect("usage");
    assert!(second_usage.sequence > first_usage.sequence);

    // What Nomad does not do is refused, not approximated.
    assert!(matches!(
        backend.put_file(&workload, "note.txt", b"x"),
        Err(aseman_ports::PortError::Unsupported(_))
    ));
    assert!(matches!(
        backend.verify("docker", "{}"),
        Err(aseman_ports::PortError::Unsupported(_))
    ));

    // Stop leaves the job registered with no allocation.
    workload.desired.state = aseman_domain::DesiredWorkloadState::Stopped;
    workload.desired.generation = workload.desired.generation.next().expect("generation");
    let stopped = backend
        .step(&workload, ReconcileAction::Stop)
        .expect("stop");
    assert_eq!(stopped.state, ObservedWorkloadState::Stopped);
    assert_eq!(stopped.generation, workload.desired.generation);
    // A plain observation of a stopped workload agrees: the backend reads the job's
    // own state, so "no allocation" is not mistaken for "still placing".
    until(
        "the workload to observe as stopped",
        Duration::from_secs(60),
        || {
            let observation = backend.step(&workload, ReconcileAction::None)?;
            Ok((observation.state == ObservedWorkloadState::Stopped).then(|| "stopped".to_owned()))
        },
    );

    // Delete purges it; a repeated delete is harmless and observation forgets it.
    workload.desired.state = aseman_domain::DesiredWorkloadState::Deleted;
    workload.desired.generation = workload.desired.generation.next().expect("generation");
    backend
        .step(&workload, ReconcileAction::Delete)
        .expect("delete");
    backend
        .step(&workload, ReconcileAction::Delete)
        .expect("a repeated delete");
    until("the job to disappear", Duration::from_secs(60), || {
        let all = backend.observe_all()?;
        Ok(all
            .iter()
            .all(|(id, _)| *id != workload.id)
            .then(|| "gone".to_owned()))
    });
}

/// The Phase 6 gate: the Nomad provider passes the same A504 suite as the native one.
#[test]
fn the_nomad_backend_passes_the_backend_contract() {
    let Some(nomad) = cluster() else {
        eprintln!("no Nomad cluster at {}; skipped", endpoint());
        return;
    };
    assert!(build_image(), "the test image builds on this worker");
    let backend = backend(nomad);

    let mut workload = sample_workload("node:test", WorkloadId::new(), "docker");
    workload.spec.artifact = Artifact {
        kind: ArtifactKind::Oci,
        reference: IMAGE.to_owned(),
        digest: format!("sha256:{}", "0".repeat(64)),
    };
    // An invocation reaches a workload through its declared ingress, so the
    // conformance workload declares one.
    workload.spec.network.ingress = vec![IngressPort {
        name: "http".to_owned(),
        port: 8080,
        protocol: PortProtocol::Http,
    }];

    // The kit starts the workload and calls straight through; give the allocation
    // the moment Nomad needs to place it before the data-plane calls begin.
    backend
        .step(&workload, ReconcileAction::Start)
        .expect("start");
    until("the allocation to run", Duration::from_secs(120), || {
        let observation = backend.step(&workload, ReconcileAction::None)?;
        Ok((observation.state == ObservedWorkloadState::Running).then(|| "running".to_owned()))
    });

    check_backend(&backend, workload, "{\"kind\":\"signal\",\"key\":\"tick\"}");
}

/// Deny-by-default egress is enforced by the network, not assumed by the mapping
/// (A406, P6-04). The workload tries to reach the internet and says what happened.
#[test]
fn a_workload_on_the_restricted_network_cannot_reach_the_internet() {
    let Some(nomad) = cluster() else {
        eprintln!("no Nomad cluster at {}; skipped", endpoint());
        return;
    };
    // An image that reports what it could reach, then stays up so its logs can be
    // read. `wget` on an unroutable address fails fast without DNS.
    const PROBE: &str = "aseman-nomad-egress-probe:1";
    let dockerfile = "FROM busybox:1.36\n\
         CMD sh -c 'if wget -q -T 3 -O /dev/null http://1.1.1.1/; \
         then echo EGRESS-REACHED; else echo EGRESS-DENIED; fi; sleep 600'\n";
    {
        use std::io::Write;
        let mut child = std::process::Command::new("docker")
            .args(["build", "-q", "-t", PROBE, "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("docker build");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(dockerfile.as_bytes())
            .expect("the dockerfile");
        assert!(child.wait().expect("docker build").success());
    }

    let backend = backend(nomad);
    let mut workload = sample_workload("node:test", WorkloadId::new(), "docker");
    workload.spec.artifact = Artifact {
        kind: ArtifactKind::Oci,
        reference: PROBE.to_owned(),
        digest: format!("sha256:{}", "0".repeat(64)),
    };

    backend
        .step(&workload, ReconcileAction::Start)
        .expect("start");
    let verdict = until("the probe's verdict", Duration::from_secs(180), || {
        let records = backend.logs(&workload, 0, 200)?;
        Ok(records.iter().find_map(|record| {
            record
                .line
                .contains("EGRESS-")
                .then(|| record.line.trim().to_owned())
        }))
    });
    assert_eq!(
        verdict, "EGRESS-DENIED",
        "a workload whose policy denies egress must not reach the internet"
    );

    workload.desired.state = aseman_domain::DesiredWorkloadState::Deleted;
    backend
        .step(&workload, ReconcileAction::Delete)
        .expect("delete");
}
