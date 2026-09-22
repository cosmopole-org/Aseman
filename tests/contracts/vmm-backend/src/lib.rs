//! The A504 conformance kit. [`check_backend`] runs one workload through its life on
//! a backend and checks what every backend must do, whatever its infrastructure.
#![forbid(unsafe_code)]

use aseman_domain::vmm::{
    OperationKind, OperationRecord, ReconcileAction, WorkloadOperation, WorkloadRecord,
};
use aseman_domain::{ObservedWorkloadState, OperationId, OperationState};
use aseman_ports::PortError;
use aseman_ports::vmm::VmmBackend;

fn operation(record: &WorkloadRecord, kind: OperationKind, request: &str) -> OperationRecord {
    OperationRecord {
        owner: record.owner.clone(),
        id: OperationId::new(),
        workload_id: Some(record.id),
        kind,
        state: OperationState::Running,
        generation: None,
        request: Some(request.to_owned()),
        created_at_millis: 0,
        updated_at_millis: 0,
        deadline_millis: None,
        result: None,
        error: None,
    }
}

/// Run `workload` (desired running at its generation) through start, invocation,
/// logs, usage, files, stop, and delete on `backend`. `invocation` is an A501
/// `Invocation` the workload's program accepts.
///
/// # Panics
///
/// Panics when the backend deviates from A504.
pub fn check_backend(backend: &dyn VmmBackend, mut workload: WorkloadRecord, invocation: &str) {
    let description = backend.describe().expect("describe");
    assert_eq!(description.contract, "1", "A504 version");
    let runtime = description
        .runtimes
        .iter()
        .find(|runtime| runtime.runtime == workload.spec.runtime)
        .unwrap_or_else(|| panic!("{} is not offered", workload.spec.runtime))
        .clone();
    assert!(
        !runtime.deploy.entity_file_name.is_empty(),
        "deploy conventions are declared"
    );

    // Start: the observation is for the desired generation, and observe_all agrees.
    let generation = workload.desired.generation;
    let started = backend
        .step(&workload, ReconcileAction::Start)
        .expect("start");
    assert_eq!(started.generation, generation);
    assert!(
        matches!(
            started.state,
            ObservedWorkloadState::Running | ObservedWorkloadState::Pending
        ),
        "started: {:?}",
        started.state
    );
    let observed = backend.observe_all().expect("observe_all");
    let (_, current) = observed
        .iter()
        .find(|(id, _)| *id == workload.id)
        .expect("a started workload is observed");
    assert!(
        current.sequence >= started.sequence,
        "sequences move forward"
    );

    // An invocation runs when the runtime takes invocations; otherwise it is refused.
    let invoke = operation(&workload, OperationKind::Invoke, invocation);
    match (
        runtime.check(WorkloadOperation::Invoke),
        backend.run(Some(&workload), &invoke),
    ) {
        (Ok(()), Ok(result)) => {
            serde_json::from_str::<serde_json::Value>(&result).expect("an A501 result is JSON");
        }
        (Ok(()), Err(error)) => panic!("invocation failed: {error}"),
        (Err(_), outcome) => assert!(outcome.is_err(), "an unsupported invocation is refused"),
    }

    // Logs are ordered and `after` is honored.
    let logs = backend.logs(&workload, 0, 1_000).expect("logs");
    assert!(
        logs.windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence),
        "log sequences increase"
    );
    if let Some(first) = logs.first() {
        assert!(
            backend
                .logs(&workload, first.sequence, 1_000)
                .expect("logs after")
                .iter()
                .all(|record| record.sequence > first.sequence)
        );
    }

    let first_usage = backend.usage(&workload).expect("usage");
    let second_usage = backend.usage(&workload).expect("usage");
    assert!(
        second_usage.sequence >= first_usage.sequence,
        "usage is cumulative"
    );

    // Files: a missing file is not found; without the capability, refused.
    match backend.get_file(&workload, "aseman-conformance/missing") {
        Err(PortError::NotFound) => assert!(runtime.files),
        Err(PortError::Unsupported(_)) => assert!(!runtime.files),
        other => panic!("a missing file: {other:?}"),
    }

    // Stop at a newer generation.
    workload.desired.generation = generation.next().expect("generation");
    let stopped = backend
        .step(&workload, ReconcileAction::Stop)
        .expect("stop");
    assert_eq!(stopped.generation, workload.desired.generation);
    assert_eq!(stopped.state, ObservedWorkloadState::Stopped);
    assert!(stopped.sequence > started.sequence || stopped.generation != started.generation);

    // Delete: the instance is gone from observation, or observed stopped.
    workload.desired.generation = workload.desired.generation.next().expect("generation");
    let deleted = backend
        .step(&workload, ReconcileAction::Delete)
        .expect("delete");
    assert_eq!(deleted.generation, workload.desired.generation);
    let remaining = backend.observe_all().expect("observe_all");
    assert!(
        remaining
            .iter()
            .find(|(id, _)| *id == workload.id)
            .is_none_or(|(_, observation)| observation.state == ObservedWorkloadState::Stopped)
    );
    // Deleting again is harmless.
    backend
        .step(&workload, ReconcileAction::Delete)
        .expect("a repeated delete");
}
