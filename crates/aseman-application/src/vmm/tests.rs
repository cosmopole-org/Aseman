use std::sync::atomic::{AtomicI64, Ordering};

use aseman_domain::vmm::{DeployConventions, RuntimeCapabilities};
use aseman_ports::conformance::vmm::{
    MemoryVmmStores, ScriptedBackend, sample_workload, vmm_stores,
};

use super::*;

struct Clock(AtomicI64);

impl ClockPort for Clock {
    fn unix_millis(&self) -> i64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

fn runtimes() -> Vec<RuntimeCapabilities> {
    vec![
        RuntimeCapabilities {
            runtime: "wasm".to_owned(),
            invocation: true,
            build: true,
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
            pause: true,
            exec: true,
            files: true,
            http_ingress: true,
            ..RuntimeCapabilities::default()
        },
    ]
}

struct World {
    stores: MemoryVmmStores,
    backend: ScriptedBackend,
    clock: Clock,
}

impl World {
    fn new() -> Self {
        Self {
            stores: MemoryVmmStores::default(),
            backend: ScriptedBackend::new(runtimes()),
            clock: Clock(AtomicI64::new(1_000)),
        }
    }

    fn service(&self) -> VmmService<'_> {
        VmmService {
            workloads: &self.stores,
            operations: &self.stores,
            events: &self.stores,
            backend: &self.backend,
            clock: &self.clock,
        }
    }
}

fn generation(value: u64) -> Generation {
    Generation::from_stored(value).unwrap()
}

fn new_workload(runtime: &str) -> NewWorkload {
    let record = sample_workload("node-a", WorkloadId::new(), runtime);
    NewWorkload {
        id: record.id,
        labels: record.labels,
        spec: record.spec,
        desired: record.desired,
    }
}

/// Run the executor until nothing is left.
fn drain(service: &VmmService<'_>) {
    for _ in 0..10 {
        service.execute_pending(100).unwrap();
    }
}

#[test]
fn memory_reference_stores_pass_the_suite() {
    let stores = MemoryVmmStores::default();
    vmm_stores(&stores, &stores, &stores, &stores);
}

#[test]
fn a_created_workload_converges_and_commands_follow_generations() {
    let world = World::new();
    let service = world.service();
    let workload = new_workload("docker");
    let created = service.create("node-a", &workload, None).unwrap();
    assert_eq!(created.kind, OperationKind::Create);
    assert_eq!(
        service
            .create("node-a", &workload, None)
            .unwrap_err()
            .failure,
        VmmFailure::AlreadyExists
    );
    drain(&service);
    assert_eq!(
        service.operation("node-a", created.id).unwrap().state,
        OperationState::Succeeded
    );
    let record = service.workload("node-a", workload.id).unwrap();
    assert_eq!(record.applied_generation, Some(generation(1)));
    assert_eq!(
        record.observed.as_ref().map(|observed| observed.state),
        Some(ObservedWorkloadState::Running)
    );
    // Another node never sees it.
    assert_eq!(
        service.workload("node-b", workload.id).unwrap_err().failure,
        VmmFailure::NotFound
    );

    let paused = service
        .command(
            "node-a",
            workload.id,
            LifecycleCommand::Pause,
            generation(2),
            None,
            None,
        )
        .unwrap();
    // The same generation again is a replay of the same operation.
    let replay = service
        .command(
            "node-a",
            workload.id,
            LifecycleCommand::Pause,
            generation(2),
            None,
            None,
        )
        .unwrap();
    assert_eq!(replay.id, paused.id);
    let stale = service
        .command(
            "node-a",
            workload.id,
            LifecycleCommand::Stop,
            generation(1),
            None,
            None,
        )
        .unwrap_err();
    assert_eq!(stale.failure, VmmFailure::StaleGeneration);
    assert_eq!(stale.current_generation, Some(generation(2)));
    let version = service
        .workload("node-a", workload.id)
        .unwrap()
        .resource_version;
    assert_eq!(
        service
            .command(
                "node-a",
                workload.id,
                LifecycleCommand::Resume,
                generation(3),
                Some(version + 7),
                None
            )
            .unwrap_err()
            .failure,
        VmmFailure::ResourceVersionMismatch
    );
    drain(&service);
    let record = service.workload("node-a", workload.id).unwrap();
    assert_eq!(
        record.observed.as_ref().map(|observed| observed.state),
        Some(ObservedWorkloadState::Paused)
    );
    service
        .command(
            "node-a",
            workload.id,
            LifecycleCommand::Delete,
            generation(3),
            None,
            None,
        )
        .unwrap();
    drain(&service);
    assert_eq!(
        service
            .command(
                "node-a",
                workload.id,
                LifecycleCommand::Start,
                generation(4),
                None,
                None
            )
            .unwrap_err()
            .failure,
        VmmFailure::WorkloadDeleted
    );
    let events = world
        .stores
        .events_after("node-a", 0, Some(workload.id), 100)
        .unwrap();
    assert!(
        events
            .events
            .iter()
            .any(|event| event.event_type == WorkloadEventType::Deleted)
    );
    assert!(
        world
            .stores
            .events_after("node-b", 0, None, 100)
            .unwrap()
            .events
            .is_empty()
    );
}

#[test]
fn unsupported_operations_are_refused_not_degraded() {
    let world = World::new();
    let service = world.service();
    let workload = new_workload("wasm");
    service.create("node-a", &workload, None).unwrap();
    drain(&service);
    assert_eq!(
        service
            .command(
                "node-a",
                workload.id,
                LifecycleCommand::Pause,
                generation(2),
                None,
                None
            )
            .unwrap_err()
            .failure,
        VmmFailure::UnsupportedOperation
    );
    let target = OperationTarget::Workload(workload.id);
    assert_eq!(
        service
            .submit(
                "node-a",
                &target,
                WorkloadOperation::Exec,
                "{}".to_owned(),
                None
            )
            .unwrap_err()
            .failure,
        VmmFailure::UnsupportedOperation
    );
    assert_eq!(
        service
            .forward_http("node-a", workload.id, "{}")
            .unwrap_err()
            .failure,
        VmmFailure::UnsupportedOperation
    );
    assert_eq!(
        service.runtime("qemu").unwrap_err().failure,
        VmmFailure::UnsupportedOperation
    );
    let mut with_ports = new_workload("wasm");
    with_ports
        .spec
        .network
        .ingress
        .push(aseman_domain::vmm::IngressPort {
            name: "web".to_owned(),
            port: 80,
            protocol: aseman_domain::vmm::PortProtocol::Http,
        });
    assert_eq!(
        service
            .create("node-a", &with_ports, None)
            .unwrap_err()
            .failure,
        VmmFailure::UnsupportedOperation
    );
}

#[test]
fn invocations_run_once_and_need_a_running_workload() {
    let world = World::new();
    let service = world.service();
    let workload = new_workload("wasm");
    service.create("node-a", &workload, None).unwrap();
    drain(&service);
    let target = OperationTarget::Workload(workload.id);
    let invocation = service
        .submit(
            "node-a",
            &target,
            WorkloadOperation::Invoke,
            "{\"kind\":\"signal\"}".to_owned(),
            None,
        )
        .unwrap();
    drain(&service);
    let done = service.operation("node-a", invocation.id).unwrap();
    assert_eq!(done.state, OperationState::Succeeded);
    assert_eq!(
        done.result.as_deref(),
        Some("{\"output\":{\"kind\":\"signal\"}}")
    );
    // A backend failure fails the operation with a problem code.
    world
        .backend
        .fail_next(PortError::Failed("module trapped".to_owned()));
    let failing = service
        .submit(
            "node-a",
            &target,
            WorkloadOperation::Invoke,
            "{}".to_owned(),
            None,
        )
        .unwrap();
    drain(&service);
    let failed = service.operation("node-a", failing.id).unwrap();
    assert_eq!(failed.state, OperationState::Failed);
    assert_eq!(
        failed.error.map(|error| error.code),
        Some(VmmFailure::BackendFailure)
    );
    service
        .command(
            "node-a",
            workload.id,
            LifecycleCommand::Stop,
            generation(2),
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        service
            .submit(
                "node-a",
                &target,
                WorkloadOperation::Invoke,
                "{}".to_owned(),
                None
            )
            .unwrap_err()
            .failure,
        VmmFailure::InvalidTransition
    );
    // Builds name a runtime.
    let build = service
        .submit(
            "node-a",
            &OperationTarget::Runtime("wasm".to_owned()),
            WorkloadOperation::Build,
            "{}".to_owned(),
            None,
        )
        .unwrap();
    assert_eq!(build.workload_id, None);
}

#[test]
fn deadlines_and_cancellation_finish_operations() {
    let world = World::new();
    let service = world.service();
    let workload = new_workload("docker");
    service.create("node-a", &workload, Some(0)).unwrap();
    drain(&service);
    let operations = service
        .operations("node-a", &OperationFilter::default(), None, 10)
        .unwrap();
    assert_eq!(
        operations.items[0].error.as_ref().map(|error| error.code),
        Some(VmmFailure::DeadlineExceeded)
    );
    let started = service
        .command(
            "node-a",
            workload.id,
            LifecycleCommand::Start,
            generation(2),
            None,
            None,
        )
        .unwrap();
    let cancelled = service.cancel("node-a", started.id).unwrap();
    assert_eq!(cancelled.state, OperationState::Cancelled);
    assert_eq!(
        service.cancel("node-a", started.id).unwrap_err().failure,
        VmmFailure::OperationFinished
    );
    assert_eq!(
        service.execute(started.id, "node-a").unwrap().state,
        OperationState::Cancelled
    );
}

#[test]
fn a_superseded_operation_fails_and_the_newer_generation_wins() {
    let world = World::new();
    let service = world.service();
    let workload = new_workload("docker");
    service.create("node-a", &workload, None).unwrap();
    let stop = service
        .command(
            "node-a",
            workload.id,
            LifecycleCommand::Stop,
            generation(2),
            None,
            None,
        )
        .unwrap();
    service
        .command(
            "node-a",
            workload.id,
            LifecycleCommand::Start,
            generation(3),
            None,
            None,
        )
        .unwrap();
    drain(&service);
    let stop = service.operation("node-a", stop.id).unwrap();
    assert_eq!(stop.state, OperationState::Failed);
    assert_eq!(
        stop.error.map(|error| error.code),
        Some(VmmFailure::StaleGeneration)
    );
    let record = service.workload("node-a", workload.id).unwrap();
    assert_eq!(record.applied_generation, Some(generation(3)));
    assert_eq!(
        record.observed.map(|observed| observed.state),
        Some(ObservedWorkloadState::Running)
    );
}

#[test]
fn a_transient_backend_failure_is_retried() {
    let world = World::new();
    let service = world.service();
    let workload = new_workload("docker");
    let created = service.create("node-a", &workload, None).unwrap();
    world.backend.fail_next(PortError::Unavailable("docker"));
    service.execute_pending(10).unwrap();
    assert_eq!(
        service.operation("node-a", created.id).unwrap().state,
        OperationState::Running
    );
    drain(&service);
    assert_eq!(
        service.operation("node-a", created.id).unwrap().state,
        OperationState::Succeeded
    );
}

#[test]
fn lost_instances_are_restarted_and_undesired_ones_left_alone() {
    let world = World::new();
    let service = world.service();
    let workload = new_workload("docker");
    service.create("node-a", &workload, None).unwrap();
    drain(&service);
    let stranger = WorkloadId::new();
    world.backend.plant(
        stranger,
        Observation {
            state: ObservedWorkloadState::Running,
            generation: generation(1),
            sequence: 1,
            reason: None,
            observed_at_millis: 0,
        },
    );
    world.backend.lose(workload.id);
    let report = service.observe().unwrap();
    assert_eq!(report.lost, 1);
    assert_eq!(report.undesired, vec![stranger]);
    assert_eq!(
        service
            .workload("node-a", workload.id)
            .unwrap()
            .observed
            .map(|observed| observed.state),
        Some(ObservedWorkloadState::Lost)
    );
    assert_eq!(service.reconcile().unwrap(), 1);
    // Already open: not opened twice.
    assert_eq!(service.reconcile().unwrap(), 0);
    drain(&service);
    assert!(
        world
            .backend
            .steps()
            .contains(&(workload.id, ReconcileAction::Restart))
    );
    assert!(!world.backend.steps().iter().any(|(id, _)| *id == stranger));
    assert_eq!(service.reconcile().unwrap(), 0);
    // A second observation of the same state is recorded in order.
    assert_eq!(service.observe().unwrap().recorded, 1);
}

#[test]
fn files_need_a_clean_relative_path() {
    let world = World::new();
    let service = world.service();
    let workload = new_workload("docker");
    service.create("node-a", &workload, None).unwrap();
    for path in ["", "/etc/passwd", "a/../b", "./a", "a//b", "a\\b"] {
        assert_eq!(
            service
                .put_file("node-a", workload.id, path, b"x")
                .unwrap_err()
                .failure,
            VmmFailure::InvalidRequest,
            "{path}"
        );
    }
    service
        .put_file("node-a", workload.id, "app/config.json", b"{}")
        .unwrap();
    assert_eq!(
        service
            .get_file("node-a", workload.id, "app/config.json")
            .unwrap(),
        b"{}"
    );
}
