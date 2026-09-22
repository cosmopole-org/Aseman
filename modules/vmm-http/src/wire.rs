//! Conversions between the VMM records and the A501 wire types.

use aseman_contracts::vmm::{
    Operation, OperationResult, Problem, ProblemCode, Workload, WorkloadEvent,
};
use aseman_domain::vmm::{OperationFailure, OperationRecord, WorkloadEventRecord, WorkloadRecord};
use aseman_domain::{OperationId, WorkloadId};
use aseman_ports::PortError;
use aseman_ports::vmm::BackendDescription;

/// A short human title for each code; clients branch on the code only.
#[must_use]
pub fn title(code: ProblemCode) -> &'static str {
    match code {
        ProblemCode::InvalidRequest => "The request is invalid",
        ProblemCode::Unauthenticated => "The client is not authenticated",
        ProblemCode::Forbidden => "The client may not do this",
        ProblemCode::NotFound => "Not found",
        ProblemCode::AlreadyExists => "It already exists",
        ProblemCode::ResourceVersionMismatch => "The resource version changed",
        ProblemCode::StaleGeneration => "A newer generation was applied",
        ProblemCode::InvalidTransition => "The state transition is not allowed",
        ProblemCode::WorkloadDeleted => "The workload is deleted",
        ProblemCode::UnsupportedOperation => "The runtime does not support this",
        ProblemCode::IdempotencyKeyReused => "The idempotency key was used for another request",
        ProblemCode::IdempotencyInProgress => "A request with this idempotency key is running",
        ProblemCode::OperationFinished => "The operation already finished",
        ProblemCode::PayloadTooLarge => "The request is too large",
        ProblemCode::RateLimited => "Too many requests",
        ProblemCode::DeadlineExceeded => "The deadline passed",
        ProblemCode::Unavailable => "The VMM is unavailable",
        ProblemCode::BackendFailure => "The backend failed",
        ProblemCode::StaleObservation => "The observation is stale",
    }
}

#[must_use]
pub fn problem(code: ProblemCode, detail: &str, request_id: &str) -> Problem {
    Problem {
        detail: (!detail.is_empty()).then(|| detail.to_owned()),
        ..Problem::new(code, title(code), request_id)
    }
}

#[must_use]
pub fn workload(record: &WorkloadRecord) -> Workload {
    Workload {
        id: *record.id.as_uuid(),
        labels: record.labels.clone(),
        spec: record.spec.redacted(),
        desired: record.desired,
        applied_generation: record.applied_generation,
        observed: record.observed.clone(),
        resource_version: record.resource_version.to_string(),
        created_at_millis: record.created_at_millis,
        updated_at_millis: record.updated_at_millis,
    }
}

/// The record a client reads back; `owner` is the client's own identity.
///
/// # Errors
///
/// `Failed` when the resource version is not a number.
pub fn workload_record(owner: &str, workload: Workload) -> Result<WorkloadRecord, PortError> {
    Ok(WorkloadRecord {
        owner: owner.to_owned(),
        id: WorkloadId::from_uuid(workload.id),
        labels: workload.labels,
        spec: workload.spec,
        desired: workload.desired,
        applied_generation: workload.applied_generation,
        observed: workload.observed,
        resource_version: workload
            .resource_version
            .parse()
            .map_err(|_| PortError::Failed("invalid resource version".to_owned()))?,
        created_at_millis: workload.created_at_millis,
        updated_at_millis: workload.updated_at_millis,
    })
}

#[must_use]
pub fn operation(record: &OperationRecord, request_id: &str) -> Operation {
    Operation {
        id: *record.id.as_uuid(),
        workload_id: record.workload_id.map(|id| *id.as_uuid()),
        kind: record.kind,
        state: record.state,
        generation: record.generation,
        created_at_millis: record.created_at_millis,
        updated_at_millis: record.updated_at_millis,
        deadline_millis: record.deadline_millis,
        result: record
            .result
            .as_deref()
            .and_then(|result| serde_json::from_str::<OperationResult>(result).ok()),
        error: record
            .error
            .as_ref()
            .map(|failure| problem(failure.code, &failure.detail, request_id)),
    }
}

#[must_use]
pub fn operation_record(owner: &str, operation: Operation) -> OperationRecord {
    OperationRecord {
        owner: owner.to_owned(),
        id: OperationId::from_uuid(operation.id),
        workload_id: operation.workload_id.map(WorkloadId::from_uuid),
        kind: operation.kind,
        state: operation.state,
        generation: operation.generation,
        request: None,
        created_at_millis: operation.created_at_millis,
        updated_at_millis: operation.updated_at_millis,
        deadline_millis: operation.deadline_millis,
        result: operation
            .result
            .and_then(|result| serde_json::to_string(&result).ok()),
        error: operation.error.map(|problem| OperationFailure {
            code: problem.code,
            detail: problem.detail.unwrap_or_default(),
        }),
    }
}

#[must_use]
pub fn event(record: &WorkloadEventRecord, operation: Option<Operation>) -> WorkloadEvent {
    WorkloadEvent {
        sequence: record.sequence,
        workload_id: *record.workload_id.as_uuid(),
        at_millis: record.at_millis,
        event_type: record.event_type,
        observation: record.observation.clone(),
        operation,
    }
}

#[must_use]
pub fn event_record(owner: &str, event: WorkloadEvent) -> WorkloadEventRecord {
    WorkloadEventRecord {
        owner: owner.to_owned(),
        sequence: event.sequence,
        workload_id: WorkloadId::from_uuid(event.workload_id),
        at_millis: event.at_millis,
        event_type: event.event_type,
        observation: event.observation,
        operation: event
            .operation
            .map(|operation| OperationId::from_uuid(operation.id)),
    }
}

#[must_use]
pub fn capabilities(
    description: BackendDescription,
    max_request_bytes: u64,
) -> aseman_contracts::vmm::Capabilities {
    aseman_contracts::vmm::Capabilities {
        api_version: aseman_contracts::vmm::API_VERSION.to_owned(),
        backend: aseman_contracts::vmm::BackendInfo {
            name: description.name,
            version: description.version,
        },
        runtimes: description.runtimes,
        max_request_bytes,
    }
}
