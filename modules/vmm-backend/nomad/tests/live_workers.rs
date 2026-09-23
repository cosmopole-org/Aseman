//! Worker lifecycle on a real Nomad cluster (P6-03, A606): cordon stops placement,
//! drain moves work off, a cancelled drain does not silently return the worker to
//! service, and none of it changes the workload's identity or generation.
//!
//! Skipped when no cluster answers. Aseman never installs one (ADR 0002).

use std::time::{Duration, Instant};

use aseman_domain::vmm::{Artifact, ArtifactKind, ReconcileAction};
use aseman_domain::{ObservedWorkloadState, WorkloadId};
use aseman_ports::conformance::vmm::sample_workload;
use aseman_ports::vmm::VmmBackend;
use aseman_vmm_backend_nomad::backend::{NomadBackend, Runtime, runtime_capabilities};
use aseman_vmm_backend_nomad::client::Nomad;
use aseman_vmm_backend_nomad::job::{Execution, NetworkMode};
use aseman_vmm_backend_nomad::workers::Workers;

/// A workload that stays up: a bare busybox exits at once, and a workload that keeps
/// restarting would be indistinguishable from one the cordon disturbed.
const IMAGE: &str = "aseman-nomad-worker-test:1";

const DOCKERFILE: &str = "FROM busybox:1.36\nCMD [\"sleep\", \"86400\"]\n";

/// Build the image at most once per test binary: the tests run in parallel and two
/// concurrent `docker build` calls for the same tag race each other.
fn build_image() -> bool {
    static BUILT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *BUILT.get_or_init(build_image_once)
}

fn build_image_once() -> bool {
    use std::io::Write;
    let Ok(mut child) = std::process::Command::new("docker")
        .args(["build", "-q", "-t", IMAGE, "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    if child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(DOCKERFILE.as_bytes())
        .is_err()
    {
        return false;
    }
    child.wait().is_ok_and(|status| status.success())
}

/// Puts the worker back the way the test found it, however the test ends. A panic
/// must not leave an operator with a cordoned machine and no idea why.
struct Restore<'a> {
    workers: &'a Workers,
    id: String,
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        let _ = self.workers.cancel_drain(&self.id);
        let _ = self.workers.uncordon(&self.id);
    }
}

fn endpoint() -> String {
    aseman_config::IntegrationTestConfig::from_process()
        .nomad_endpoint
        .unwrap_or_else(|| "http://127.0.0.1:4646".to_owned())
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

fn until(what: &str, deadline: Duration, mut check: impl FnMut() -> bool) {
    let end = Instant::now() + deadline;
    while !check() {
        assert!(Instant::now() < end, "never saw {what}");
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
fn a_worker_is_cordoned_drained_and_returned_without_touching_identity() {
    let Some(nomad) = cluster() else {
        eprintln!("no Nomad cluster at {}; skipped", endpoint());
        return;
    };
    let workers = Workers::new(nomad);
    let pool = workers.list().expect("the worker pool");
    let Some(worker) = pool.iter().find(|worker| worker.accepting()).cloned() else {
        eprintln!("no accepting worker; skipped");
        return;
    };

    assert!(build_image(), "the test image builds on this worker");
    let _restore = Restore {
        workers: &workers,
        id: worker.id.clone(),
    };

    let backend = NomadBackend::start(
        cluster().expect("a second handle"),
        vec![Runtime {
            capabilities: runtime_capabilities("docker", &Execution::Image)
                .expect("the docker runtime is in the A505 matrix"),
            execution: Execution::Image,
        }],
        vec![worker.datacenter.clone()],
        restricted(),
    )
    .expect("the cluster answers");

    let mut workload = sample_workload("node:test", WorkloadId::new(), "docker");
    workload.spec.artifact = Artifact {
        kind: ArtifactKind::Oci,
        reference: IMAGE.to_owned(),
        digest: format!("sha256:{}", "0".repeat(64)),
    };
    workload.spec.entry = "main".to_owned();
    let identity = (workload.id, workload.desired.generation);

    backend
        .step(&workload, ReconcileAction::Start)
        .expect("start");
    until("the workload running", Duration::from_secs(120), || {
        backend
            .step(&workload, ReconcileAction::None)
            .is_ok_and(|observation| observation.state == ObservedWorkloadState::Running)
    });

    // Cordon: the worker stops accepting, and what already runs is left alone.
    workers.cordon(&worker.id).expect("cordon");
    let cordoned = workers.get(&worker.id).expect("the worker");
    assert!(!cordoned.eligible, "a cordoned worker takes no new work");
    assert!(!cordoned.accepting());
    assert_eq!(
        backend
            .step(&workload, ReconcileAction::None)
            .expect("observe")
            .state,
        ObservedWorkloadState::Running,
        "a cordon does not disturb what already runs"
    );

    // Drain: the work leaves. With one worker there is nowhere to go, so the
    // workload stops — which is the honest observation, not a pretended success.
    workers.drain(&worker.id, 10_000).expect("drain");
    until("the worker drained", Duration::from_secs(120), || {
        workers.drained(&worker.id).unwrap_or(false)
    });
    let draining = workers.get(&worker.id).expect("the worker");
    assert!(draining.draining || !draining.eligible);
    let observed = backend
        .step(&workload, ReconcileAction::None)
        .expect("observe");
    assert_ne!(
        observed.state,
        ObservedWorkloadState::Running,
        "a drained worker runs nothing"
    );

    // Through all of it, the workload is the same workload.
    assert_eq!(
        (workload.id, observed.generation),
        identity,
        "draining a worker never changes a workload's identity or generation"
    );

    // Cancelling the drain leaves the worker cordoned: a suspect machine is not put
    // back into rotation by accident.
    workers.cancel_drain(&worker.id).expect("cancel");
    until("the drain to stop", Duration::from_secs(60), || {
        workers.get(&worker.id).is_ok_and(|worker| !worker.draining)
    });
    assert!(
        !workers.get(&worker.id).expect("the worker").eligible,
        "a cancelled drain does not uncordon"
    );

    // The operator says so explicitly, and the workload comes back on its own
    // identity.
    workers.uncordon(&worker.id).expect("uncordon");
    until(
        "the worker accepting again",
        Duration::from_secs(60),
        || {
            workers
                .get(&worker.id)
                .is_ok_and(|worker| worker.accepting())
        },
    );
    backend
        .step(&workload, ReconcileAction::Start)
        .expect("restart");
    until(
        "the workload running again",
        Duration::from_secs(120),
        || {
            backend
                .step(&workload, ReconcileAction::None)
                .is_ok_and(|observation| observation.state == ObservedWorkloadState::Running)
        },
    );
    let back = backend
        .step(&workload, ReconcileAction::None)
        .expect("observe");
    assert_eq!((workload.id, back.generation), identity);

    // Clean up: the test must leave the cluster as it found it.
    workload.desired.state = aseman_domain::DesiredWorkloadState::Deleted;
    backend
        .step(&workload, ReconcileAction::Delete)
        .expect("delete");
}

/// A workload whose container dies is observed as not running, and the reconciler's
/// restart brings it back as the same workload (A503, A606).
#[test]
fn a_lost_workload_is_observed_and_restarted_as_itself() {
    let Some(nomad) = cluster() else {
        eprintln!("no Nomad cluster at {}; skipped", endpoint());
        return;
    };
    assert!(build_image(), "the test image builds on this worker");
    let backend = NomadBackend::start(
        nomad,
        vec![Runtime {
            capabilities: runtime_capabilities("docker", &Execution::Image)
                .expect("the docker runtime is in the A505 matrix"),
            execution: Execution::Image,
        }],
        vec!["dc1".to_owned()],
        restricted(),
    )
    .expect("the cluster answers");

    let mut workload = sample_workload("node:test", WorkloadId::new(), "docker");
    workload.spec.artifact = Artifact {
        kind: ArtifactKind::Oci,
        reference: IMAGE.to_owned(),
        digest: format!("sha256:{}", "0".repeat(64)),
    };
    let identity = (workload.id, workload.desired.generation);

    backend
        .step(&workload, ReconcileAction::Start)
        .expect("start");
    until("the workload running", Duration::from_secs(120), || {
        backend
            .step(&workload, ReconcileAction::None)
            .is_ok_and(|observation| observation.state == ObservedWorkloadState::Running)
    });

    // Kill the container out from under the scheduler: the same thing a worker's
    // kernel does when it runs out of memory.
    let killed = std::process::Command::new("docker")
        .args([
            "ps",
            "--filter",
            "name=^/workload-",
            "--format",
            "{{.Names}}",
        ])
        .output()
        .expect("docker ps");
    let name = String::from_utf8_lossy(&killed.stdout)
        .lines()
        .next()
        .map(str::to_owned)
        .expect("the workload's container");
    std::process::Command::new("docker")
        .args(["rm", "-f", &name])
        .output()
        .expect("docker rm");

    // The backend reports what is true, not what was asked for.
    until(
        "the workload to stop running",
        Duration::from_secs(120),
        || {
            backend
                .step(&workload, ReconcileAction::None)
                .is_ok_and(|observation| observation.state != ObservedWorkloadState::Running)
        },
    );

    // Reconciliation's restart is a start at the same generation. The workload comes
    // back as itself: same ID, same generation, no new workload invented.
    backend
        .step(&workload, ReconcileAction::Restart)
        .expect("restart");
    until(
        "the workload running again",
        Duration::from_secs(120),
        || {
            backend
                .step(&workload, ReconcileAction::None)
                .is_ok_and(|observation| observation.state == ObservedWorkloadState::Running)
        },
    );
    let back = backend
        .step(&workload, ReconcileAction::None)
        .expect("observe");
    assert_eq!((workload.id, back.generation), identity);

    workload.desired.state = aseman_domain::DesiredWorkloadState::Deleted;
    backend
        .step(&workload, ReconcileAction::Delete)
        .expect("delete");
}
