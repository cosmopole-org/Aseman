//! VMM lifecycle rules (A502, A503): desired-state transitions, the operation state
//! machine, generation rules for desired and observed state, reconciliation, and
//! runtime capabilities. Pure: the VMM service applies them to its stores.

use std::collections::BTreeMap;
use std::fmt;

use crate::{
    DesiredWorkloadState, Generation, ObservedWorkloadState, OperationId, OperationState,
    WorkloadId,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Why a VMM request or operation failed: the closed A501 problem code set.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmmFailure {
    InvalidRequest,
    Unauthenticated,
    Forbidden,
    NotFound,
    AlreadyExists,
    ResourceVersionMismatch,
    StaleGeneration,
    InvalidTransition,
    WorkloadDeleted,
    UnsupportedOperation,
    IdempotencyKeyReused,
    IdempotencyInProgress,
    OperationFinished,
    PayloadTooLarge,
    RateLimited,
    DeadlineExceeded,
    Unavailable,
    BackendFailure,
    StaleObservation,
}

impl VmmFailure {
    pub const ALL: [Self; 19] = [
        Self::InvalidRequest,
        Self::Unauthenticated,
        Self::Forbidden,
        Self::NotFound,
        Self::AlreadyExists,
        Self::ResourceVersionMismatch,
        Self::StaleGeneration,
        Self::InvalidTransition,
        Self::WorkloadDeleted,
        Self::UnsupportedOperation,
        Self::IdempotencyKeyReused,
        Self::IdempotencyInProgress,
        Self::OperationFinished,
        Self::PayloadTooLarge,
        Self::RateLimited,
        Self::DeadlineExceeded,
        Self::Unavailable,
        Self::BackendFailure,
        Self::StaleObservation,
    ];

    /// The code's wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Unauthenticated => "unauthenticated",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::AlreadyExists => "already_exists",
            Self::ResourceVersionMismatch => "resource_version_mismatch",
            Self::StaleGeneration => "stale_generation",
            Self::InvalidTransition => "invalid_transition",
            Self::WorkloadDeleted => "workload_deleted",
            Self::UnsupportedOperation => "unsupported_operation",
            Self::IdempotencyKeyReused => "idempotency_key_reused",
            Self::IdempotencyInProgress => "idempotency_in_progress",
            Self::OperationFinished => "operation_finished",
            Self::PayloadTooLarge => "payload_too_large",
            Self::RateLimited => "rate_limited",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::Unavailable => "unavailable",
            Self::BackendFailure => "backend_failure",
            Self::StaleObservation => "stale_observation",
        }
    }

    /// Whether a client may retry the same request unchanged.
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::IdempotencyInProgress
                | Self::RateLimited
                | Self::DeadlineExceeded
                | Self::Unavailable
                | Self::BackendFailure
        )
    }
}

impl From<LifecycleError> for VmmFailure {
    fn from(error: LifecycleError) -> Self {
        match error {
            LifecycleError::Deleted => Self::WorkloadDeleted,
            LifecycleError::InvalidTransition => Self::InvalidTransition,
            LifecycleError::StaleGeneration => Self::StaleGeneration,
            LifecycleError::StaleObservation | LifecycleError::FutureObservation => {
                Self::StaleObservation
            }
            LifecycleError::Unsupported => Self::UnsupportedOperation,
            LifecycleError::OperationFinished => Self::OperationFinished,
        }
    }
}

/// Why a lifecycle change is refused.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleError {
    #[error("the workload is deleted")]
    Deleted,
    #[error("the transition is not allowed")]
    InvalidTransition,
    #[error("the expected generation is stale")]
    StaleGeneration,
    #[error("the observation is stale")]
    StaleObservation,
    #[error("the observation claims a generation that was never desired")]
    FutureObservation,
    #[error("the runtime does not support this operation")]
    Unsupported,
    #[error("the operation already finished")]
    OperationFinished,
}

/// The desired-state transitions (A502). `deleted` is terminal; `paused` is reachable
/// only from `running`; setting the current state again is a no-op, not an error.
///
/// # Errors
///
/// `Deleted` or `InvalidTransition`.
pub fn desired_transition(
    from: DesiredWorkloadState,
    to: DesiredWorkloadState,
) -> Result<(), LifecycleError> {
    use DesiredWorkloadState::{Deleted, Paused, Running, Stopped};
    match (from, to) {
        (Deleted, _) => Err(LifecycleError::Deleted),
        (current, next) if current == next => Ok(()),
        (Running, Paused)
        | (Paused, Running)
        | (_, Stopped)
        | (Stopped, Running)
        | (_, Deleted) => Ok(()),
        _ => Err(LifecycleError::InvalidTransition),
    }
}

/// Every allowed desired transition, for the published state table.
#[must_use]
pub fn desired_transitions() -> Vec<(DesiredWorkloadState, DesiredWorkloadState)> {
    use DesiredWorkloadState::{Deleted, Paused, Running, Stopped};
    let states = [Stopped, Running, Paused, Deleted];
    states
        .iter()
        .flat_map(|from| states.iter().map(move |to| (*from, *to)))
        .filter(|(from, to)| from != to && desired_transition(*from, *to).is_ok())
        .collect()
}

/// Apply a desired change under optimistic concurrency (A503): the caller names the
/// generation it read, and every accepted change advances it by one.
///
/// # Errors
///
/// `StaleGeneration`, or the transition's error.
pub fn next_desired(
    current: DesiredWorkloadState,
    current_generation: Generation,
    expected_generation: Generation,
    to: DesiredWorkloadState,
) -> Result<Generation, LifecycleError> {
    if current_generation != expected_generation {
        return Err(LifecycleError::StaleGeneration);
    }
    desired_transition(current, to)?;
    current_generation
        .next()
        .map_err(|_| LifecycleError::InvalidTransition)
}

/// How the VMM treats a command carrying the node's desired generation (A503). The
/// node owns desired state, so its generation is authoritative: a newer one applies,
/// the applied one is an idempotent replay, and an older one is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandFreshness {
    Apply,
    Replay,
    Stale,
}

/// Classify a command's generation against the last one the VMM applied.
#[must_use]
pub fn command_freshness(applied: Option<Generation>, incoming: Generation) -> CommandFreshness {
    match applied {
        Some(applied) if incoming.get() < applied.get() => CommandFreshness::Stale,
        Some(applied) if incoming == applied => CommandFreshness::Replay,
        _ => CommandFreshness::Apply,
    }
}

/// What a provider observed, for the generation of the desired state it acted on.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub state: ObservedWorkloadState,
    pub generation: Generation,
    /// Provider-local monotonic counter; ordering between observations of one
    /// generation.
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub observed_at_millis: i64,
}

/// Whether `incoming` replaces `recorded` (A503): observations never go back in
/// generation or sequence, and never claim a generation newer than desired.
///
/// # Errors
///
/// `FutureObservation` or `StaleObservation`.
pub fn accept_observation(
    desired_generation: Generation,
    recorded: Option<&Observation>,
    incoming: &Observation,
) -> Result<(), LifecycleError> {
    if incoming.generation.get() > desired_generation.get() {
        return Err(LifecycleError::FutureObservation);
    }
    match recorded {
        Some(recorded)
            if (incoming.generation.get(), incoming.sequence)
                <= (recorded.generation.get(), recorded.sequence) =>
        {
            Err(LifecycleError::StaleObservation)
        }
        _ => Ok(()),
    }
}

/// The one step reconciliation takes to move observed toward desired.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileAction {
    /// Observed already matches the desired generation.
    None,
    Start,
    Stop,
    Pause,
    Resume,
    Delete,
    /// The provider lost or failed the workload while it should run: start it again.
    Restart,
    /// An observed workload with no desired record: adopt it as stopped-desired and
    /// ask the operator, never run or delete it silently (ADR 0022).
    Adopt,
}

/// Plan the next reconciliation step (A503). `observed` is `None` when the provider
/// reports nothing for the workload.
#[must_use]
pub fn reconcile(
    desired: Option<(DesiredWorkloadState, Generation)>,
    observed: Option<&Observation>,
) -> ReconcileAction {
    use DesiredWorkloadState as D;
    use ObservedWorkloadState as O;
    let Some((desired, generation)) = desired else {
        return if observed.is_some() {
            ReconcileAction::Adopt
        } else {
            ReconcileAction::None
        };
    };
    let current = observed.filter(|observed| observed.generation == generation);
    match (desired, observed.map(|observed| observed.state), current) {
        (D::Deleted, None, _)
        // The backend acted on the deletion: the instance is gone.
        | (D::Deleted, Some(O::Stopped | O::Lost | O::Failed), Some(_)) => ReconcileAction::None,
        (D::Deleted, Some(_), _) => ReconcileAction::Delete,
        (D::Running, Some(O::Running), Some(_)) => ReconcileAction::None,
        (D::Running, Some(O::Paused), _) => ReconcileAction::Resume,
        (D::Running, Some(O::Failed | O::Lost), _) => ReconcileAction::Restart,
        (D::Running, Some(O::Pending), Some(_)) => ReconcileAction::None,
        (D::Running, _, _) => ReconcileAction::Start,
        (D::Paused, Some(O::Paused), Some(_)) => ReconcileAction::None,
        (D::Paused, Some(O::Running), _) => ReconcileAction::Pause,
        (D::Paused, _, _) => ReconcileAction::Start,
        (D::Stopped, None | Some(O::Stopped | O::Failed | O::Lost), _) => ReconcileAction::None,
        (D::Stopped, _, _) => ReconcileAction::Stop,
    }
}

/// Operation transitions (A502): `pending` then `running` then a terminal state;
/// a pending operation may be cancelled; terminal states never change.
///
/// # Errors
///
/// `OperationFinished` or `InvalidTransition`.
pub fn operation_transition(
    from: OperationState,
    to: OperationState,
) -> Result<(), LifecycleError> {
    use OperationState::{Cancelled, Failed, Pending, Running, Succeeded};
    match (from, to) {
        (Succeeded | Failed | Cancelled, _) => Err(LifecycleError::OperationFinished),
        (Pending, Running | Cancelled | Failed) | (Running, Succeeded | Failed | Cancelled) => {
            Ok(())
        }
        _ => Err(LifecycleError::InvalidTransition),
    }
}

/// What a runtime supports, negotiated before scheduling (plan 04: unsupported
/// semantics are reported, never degraded silently).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeCapabilities {
    pub runtime: String,
    /// Event-driven executions: an input runs the program once.
    pub invocation: bool,
    /// Long-running instances with a lifecycle.
    pub long_running: bool,
    pub pause: bool,
    pub snapshot: bool,
    pub exec: bool,
    pub terminal: bool,
    pub http_ingress: bool,
    pub files: bool,
    pub build: bool,
    /// Runs grouped chain transactions and effects (consensus-driven invocations).
    pub chain_transactions: bool,
    /// Returns execution proofs and verifies them.
    pub execution_proofs: bool,
    pub deploy: DeployConventions,
}

/// How a runtime's program files are deployed.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeployConventions {
    /// The name the uploaded primary file is stored under, for example `module.wasm`.
    pub entity_file_name: String,
    /// Whether a deployment may carry more files than the primary one.
    pub accepts_extra_files: bool,
    /// Whether a deployment must be built before it can run.
    pub build_on_deploy: bool,
}

/// A workload-level operation a caller may ask for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadOperation {
    Start,
    Stop,
    Pause,
    Resume,
    Delete,
    Invoke,
    InvokeChain,
    VerifyExecution,
    Exec,
    Terminal,
    HttpForward,
    CopyFiles,
    Build,
    Snapshot,
}

impl RuntimeCapabilities {
    /// Whether the runtime supports `operation`.
    ///
    /// # Errors
    ///
    /// `Unsupported`.
    pub fn check(&self, operation: WorkloadOperation) -> Result<(), LifecycleError> {
        use WorkloadOperation as W;
        let supported = match operation {
            // Every runtime has a lifecycle: for invocation runtimes `running` means
            // accepting invocations.
            W::Start | W::Stop | W::Delete => true,
            W::Pause | W::Resume => self.pause,
            W::Invoke => self.invocation,
            W::InvokeChain => self.chain_transactions,
            W::VerifyExecution => self.execution_proofs,
            W::Exec => self.exec,
            W::Terminal => self.terminal,
            W::HttpForward => self.http_ingress,
            W::CopyFiles => self.files,
            W::Build => self.build,
            W::Snapshot => self.snapshot,
        };
        supported.then_some(()).ok_or(LifecycleError::Unsupported)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Blob,
    Oci,
}

/// Content-addressed workload input.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub kind: ArtifactKind,
    pub reference: String,
    /// `sha256:{hex}`.
    pub digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    pub vcpu_millis: u64,
    pub memory_mib: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_mib: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_timeout_millis: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortProtocol {
    Http,
    Tcp,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressPort {
    pub name: String,
    pub port: u16,
    pub protocol: PortProtocol,
}

/// Deny by default in both directions.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkPolicy {
    pub ingress: Vec<IngressPort>,
    pub egress_allow: Vec<String>,
}

/// A credential that is written to the VMM and never read back, logged, or printed.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WriteOnlyCredential(String);

impl WriteOnlyCredential {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WriteOnlyCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WriteOnlyCredential(<redacted>)")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bootstrap {
    pub guest_api_url: String,
    /// Present on requests only; a VMM never returns it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<WriteOnlyCredential>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadSpec {
    pub runtime: String,
    pub artifact: Artifact,
    pub entry: String,
    pub resources: Resources,
    pub network: NetworkPolicy,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    pub bootstrap: Bootstrap,
}

impl WorkloadSpec {
    /// The spec as a VMM returns it: without the write-only credential.
    #[must_use]
    pub fn redacted(&self) -> Self {
        let mut spec = self.clone();
        spec.bootstrap.credential = None;
        spec
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadLabels {
    pub creature_id: Uuid,
    pub program_id: Uuid,
    pub entity_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_machine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_vm_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesiredStatus {
    pub state: DesiredWorkloadState,
    pub generation: Generation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Create,
    Start,
    Stop,
    Pause,
    Resume,
    Delete,
    Restart,
    UpdateSpec,
    Invoke,
    Exec,
    Build,
    Snapshot,
    Restore,
    CopyFiles,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub name: String,
    pub protocol: PortProtocol,
    pub address: String,
    pub port: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub window_start_millis: i64,
    pub window_end_millis: i64,
    pub sequence: u64,
    pub cpu_millis: u64,
    pub memory_peak_bytes: u64,
    pub network_rx_bytes: u64,
    pub network_tx_bytes: u64,
    pub storage_bytes: u64,
    pub invocations: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
    System,
    Build,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogRecord {
    pub sequence: u64,
    pub at_millis: i64,
    pub stream: LogStream,
    pub line: String,
}

/// A failed operation's code and a detail that carries no credentials or output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationFailure {
    pub code: VmmFailure,
    pub detail: String,
}

/// A workload as the VMM stores it (A501 `Workload`, plus its owner).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkloadRecord {
    /// The node that created it (its mutual-TLS identity); nobody else sees it.
    pub owner: String,
    pub id: WorkloadId,
    pub labels: WorkloadLabels,
    pub spec: WorkloadSpec,
    pub desired: DesiredStatus,
    pub applied_generation: Option<Generation>,
    pub observed: Option<Observation>,
    /// Advances on every change; the ETag is its decimal text.
    pub resource_version: u64,
    pub created_at_millis: i64,
    pub updated_at_millis: i64,
}

/// An operation as the VMM tracks it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationRecord {
    pub owner: String,
    pub id: OperationId,
    pub workload_id: Option<WorkloadId>,
    pub kind: OperationKind,
    pub state: OperationState,
    pub generation: Option<Generation>,
    /// The A501 request body a data-plane operation carries (invocation, exec, build,
    /// restore); the backend decodes it.
    pub request: Option<String>,
    pub created_at_millis: i64,
    pub updated_at_millis: i64,
    pub deadline_millis: Option<i64>,
    /// The A501 JSON result of a succeeded operation.
    pub result: Option<String>,
    pub error: Option<OperationFailure>,
}

impl OperationRecord {
    /// Move to `state`, enforcing the A502 operation machine.
    ///
    /// # Errors
    ///
    /// The transition's error.
    pub fn transition(
        &mut self,
        state: OperationState,
        at_millis: i64,
    ) -> Result<(), LifecycleError> {
        operation_transition(self.state, state)?;
        self.state = state;
        self.updated_at_millis = at_millis;
        Ok(())
    }
}

/// What the event stream carries (A501 `WorkloadEvent`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadEventType {
    Observed,
    Operation,
    Deleted,
    Resync,
}

/// One appended event; the log assigns `sequence`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkloadEventRecord {
    pub owner: String,
    pub sequence: u64,
    pub workload_id: WorkloadId,
    pub at_millis: i64,
    pub event_type: WorkloadEventType,
    pub observation: Option<Observation>,
    pub operation: Option<OperationId>,
}

#[cfg(test)]
mod tests;
