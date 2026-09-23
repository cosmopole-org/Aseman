//! Ports of the VMM service (A501, A503, A504) and of the node's VMM client.
//!
//! The VMM owns observed state and operations; the node owns desired state. Every
//! store is scoped by `owner`, the calling node's mutual-TLS identity, except the
//! reconciliation reads, which the service itself makes.

use aseman_domain::vmm::{
    DesiredStatus, Endpoint, LogRecord, Observation, OperationKind, OperationRecord,
    ReconcileAction, RuntimeCapabilities, Usage, WorkloadEventRecord, WorkloadLabels,
    WorkloadRecord, WorkloadSpec,
};
use aseman_domain::{
    Generation, ObservedWorkloadState, OperationId, OperationState, Uuid, WorkloadId,
};

use crate::PortResult;

/// One page of a cursor-paginated listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Opaque; `None` on the last page.
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkloadFilter {
    pub creature_id: Option<Uuid>,
    pub observed_state: Option<ObservedWorkloadState>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OperationFilter {
    pub workload_id: Option<WorkloadId>,
    pub state: Option<OperationState>,
}

/// Durable workload records of the VMM service.
pub trait VmmWorkloadStore: Send + Sync {
    fn workload(&self, owner: &str, id: WorkloadId) -> PortResult<Option<WorkloadRecord>>;
    /// The owner's workloads in ID order. `limit` is at least 1.
    fn workloads(
        &self,
        owner: &str,
        filter: &WorkloadFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> PortResult<Page<WorkloadRecord>>;
    /// Every owner's workloads in ID order, for reconciliation.
    fn all_workloads(&self, cursor: Option<&str>, limit: usize)
    -> PortResult<Page<WorkloadRecord>>;
    /// `Conflict` when the ID already exists, for any owner.
    fn insert_workload(&self, record: &WorkloadRecord) -> PortResult<()>;
    /// Replace the record only while its stored resource version is `expected`;
    /// otherwise `Conflict`.
    fn replace_workload(&self, record: &WorkloadRecord, expected: u64) -> PortResult<()>;
}

/// Durable operations of the VMM service.
pub trait VmmOperationStore: Send + Sync {
    fn operation(&self, owner: &str, id: OperationId) -> PortResult<Option<OperationRecord>>;
    /// The owner's operations, newest first.
    fn operations(
        &self,
        owner: &str,
        filter: &OperationFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> PortResult<Page<OperationRecord>>;
    /// `Conflict` when the ID already exists.
    fn insert_operation(&self, record: &OperationRecord) -> PortResult<()>;
    /// Replace the record only while its stored state is `expected`; otherwise
    /// `Conflict`.
    fn replace_operation(
        &self,
        record: &OperationRecord,
        expected: OperationState,
    ) -> PortResult<()>;
    /// Pending and running operations of every owner, oldest first.
    fn unfinished_operations(&self, limit: usize) -> PortResult<Vec<OperationRecord>>;
}

/// The outcome of claiming an idempotency key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdempotencyClaim {
    /// The caller owns the key and must complete or release it.
    Claimed,
    /// Another request with this key is still running.
    InProgress,
    /// The key completed; replay this response verbatim.
    Completed(ReplayableResponse),
    /// The key was used for a different request.
    Mismatch,
}

/// The response first returned for an idempotency key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayableResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: String,
    pub location: Option<String>,
}

/// Idempotency keys of mutations (A501), scoped by owner.
pub trait IdempotencyStore: Send + Sync {
    /// Claim `key` for the request whose digest is `digest`. A claim older than
    /// `claim_ttl_millis` that never completed is taken over.
    fn claim(
        &self,
        owner: &str,
        key: &str,
        digest: [u8; 32],
        now_millis: i64,
        claim_ttl_millis: i64,
    ) -> PortResult<IdempotencyClaim>;
    fn complete(&self, owner: &str, key: &str, response: &ReplayableResponse) -> PortResult<()>;
    /// Forget an unfinished claim so the request can be retried.
    fn release(&self, owner: &str, key: &str) -> PortResult<()>;
    /// Drop keys claimed before `cutoff_millis`; returns how many.
    fn purge_before(&self, cutoff_millis: i64) -> PortResult<u64>;
}

/// Events read after a position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventBatch {
    pub events: Vec<WorkloadEventRecord>,
    /// The position is older than the retained log: the reader must list again.
    pub resync: bool,
}

/// The ordered event log of the VMM service.
pub trait VmmEventLog: Send + Sync {
    /// Append and return the assigned sequence (strictly increasing, all owners).
    fn append(&self, event: &WorkloadEventRecord) -> PortResult<u64>;
    /// The owner's events with a sequence above `after`, optionally one workload's.
    fn events_after(
        &self,
        owner: &str,
        after: u64,
        workload: Option<WorkloadId>,
        limit: usize,
    ) -> PortResult<EventBatch>;
    /// Drop events recorded before `cutoff_millis`; returns how many. A reader whose
    /// position falls in the dropped range gets `resync`.
    fn truncate_before(&self, cutoff_millis: i64) -> PortResult<u64>;
}

/// A backend's identity and runtimes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendDescription {
    pub name: String,
    pub version: String,
    /// The backend contract (A504) version it speaks.
    pub contract: String,
    pub runtimes: Vec<RuntimeCapabilities>,
}

/// What a VMM backend does (A504). The service calls it only after authorizing,
/// deduplicating, and recording the request; a backend holds no Aseman state.
///
/// Data-plane requests and results are A501 JSON bodies: the service does not
/// interpret them, and the backend decodes them with `aseman-contracts`.
pub trait VmmBackend: Send + Sync {
    fn describe(&self) -> PortResult<BackendDescription>;
    /// Take one reconciliation step for the workload's desired generation and
    /// report what the instance is now.
    fn step(&self, workload: &WorkloadRecord, action: ReconcileAction) -> PortResult<Observation>;
    /// Every instance the backend runs.
    fn observe_all(&self) -> PortResult<Vec<(WorkloadId, Observation)>>;
    /// Run a data-plane operation (`invoke`, `exec`, `build`, `snapshot`, `restore`)
    /// and return its A501 result.
    fn run(
        &self,
        workload: Option<&WorkloadRecord>,
        operation: &OperationRecord,
    ) -> PortResult<String>;
    /// Forward an A501 `HttpRequest`; returns the A501 `HttpResponse`.
    fn forward_http(&self, workload: &WorkloadRecord, request: &str) -> PortResult<String>;
    fn put_file(&self, workload: &WorkloadRecord, path: &str, bytes: &[u8]) -> PortResult<()>;
    fn get_file(&self, workload: &WorkloadRecord, path: &str) -> PortResult<Vec<u8>>;
    fn endpoints(&self, workload: &WorkloadRecord) -> PortResult<Vec<Endpoint>>;
    fn usage(&self, workload: &WorkloadRecord) -> PortResult<Usage>;
    /// Log records with a sequence above `after`.
    fn logs(
        &self,
        workload: &WorkloadRecord,
        after: u64,
        limit: usize,
    ) -> PortResult<Vec<LogRecord>>;
    /// Verify an A501 `VerificationRequest`; returns the A501 `VerificationResult`.
    fn verify(&self, runtime: &str, request: &str) -> PortResult<String>;
}

/// A new workload as the node asks for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewWorkload {
    pub id: WorkloadId,
    pub labels: WorkloadLabels,
    pub spec: WorkloadSpec,
    pub desired: DesiredStatus,
}

/// A lifecycle command the node sends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleCommand {
    Start,
    Stop,
    Pause,
    Resume,
    Delete,
}

impl LifecycleCommand {
    #[must_use]
    pub const fn kind(self) -> OperationKind {
        match self {
            Self::Start => OperationKind::Start,
            Self::Stop => OperationKind::Stop,
            Self::Pause => OperationKind::Pause,
            Self::Resume => OperationKind::Resume,
            Self::Delete => OperationKind::Delete,
        }
    }
}

/// How the node reaches a VMM (A501). Every mutation carries an idempotency key
/// derived from what it does, so a retry after a crash is deduplicated.
pub trait VmmClient: Send + Sync {
    fn capabilities(&self) -> PortResult<BackendDescription>;
    fn create(&self, workload: &NewWorkload, idempotency_key: &str) -> PortResult<OperationRecord>;
    fn workload(&self, id: WorkloadId) -> PortResult<Option<WorkloadRecord>>;
    fn workloads(
        &self,
        filter: &WorkloadFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> PortResult<Page<WorkloadRecord>>;
    fn command(
        &self,
        id: WorkloadId,
        command: LifecycleCommand,
        generation: Generation,
        idempotency_key: &str,
    ) -> PortResult<OperationRecord>;
    fn update_spec(
        &self,
        id: WorkloadId,
        spec: &WorkloadSpec,
        generation: Generation,
        idempotency_key: &str,
    ) -> PortResult<OperationRecord>;
    /// Deliver an A501 `Invocation`.
    fn invoke(
        &self,
        id: WorkloadId,
        invocation: &str,
        idempotency_key: &str,
    ) -> PortResult<OperationRecord>;
    /// Forward an A501 `HttpRequest`; returns the A501 `HttpResponse`.
    fn forward_http(
        &self,
        id: WorkloadId,
        request: &str,
        idempotency_key: &str,
    ) -> PortResult<String>;
    fn operation(&self, id: OperationId) -> PortResult<Option<OperationRecord>>;
    fn events_after(&self, after: u64, limit: usize) -> PortResult<EventBatch>;
    /// Run an A501 `ExecRequest`.
    fn exec(
        &self,
        id: WorkloadId,
        request: &str,
        idempotency_key: &str,
    ) -> PortResult<OperationRecord>;
    /// Build with an A501 `BuildRequest`.
    fn build(&self, request: &str, idempotency_key: &str) -> PortResult<OperationRecord>;
    fn put_file(
        &self,
        id: WorkloadId,
        path: &str,
        bytes: &[u8],
        idempotency_key: &str,
    ) -> PortResult<()>;
    fn get_file(&self, id: WorkloadId, path: &str) -> PortResult<Vec<u8>>;
    fn endpoints(&self, id: WorkloadId) -> PortResult<Vec<Endpoint>>;
    /// Verify an A501 `VerificationRequest`; returns the A501 `VerificationResult`.
    fn verify(&self, runtime: &str, request: &str, idempotency_key: &str) -> PortResult<String>;
    /// Log records with a sequence above `after`.
    fn logs(&self, id: WorkloadId, after: u64) -> PortResult<Vec<LogRecord>>;
}
