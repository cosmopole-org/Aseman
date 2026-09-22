//! The node-to-VMM wire contract (A501) and its state tables (A502, A503).
//!
//! `contracts/vmm/openapi.json` is the source of truth for the HTTP surface; these
//! types are its Rust form, and the contract tests fail when the two disagree. The
//! lifecycle rules themselves live in `aseman_domain::vmm`.

use std::collections::BTreeMap;

pub use aseman_domain::vmm::{
    Artifact, ArtifactKind, Bootstrap, DeployConventions, DesiredStatus, Endpoint, IngressPort,
    LogRecord, LogStream, NetworkPolicy, Observation, OperationKind, PortProtocol, Resources,
    RuntimeCapabilities, Usage, VmmFailure as ProblemCode, WorkloadEventType, WorkloadLabels,
    WorkloadSpec, WriteOnlyCredential,
};
use aseman_domain::{Generation, OperationState, Uuid};
use serde::{Deserialize, Serialize};

pub const OPENAPI_JSON: &str = include_str!("../../../contracts/vmm/openapi.json");
pub const STATES_JSON: &str = include_str!("../../../contracts/vmm/states.json");
/// The contract version the node and every VMM speak.
pub const API_VERSION: &str = "1.0.0";
/// The idempotency key header, required on every mutation.
pub const IDEMPOTENCY_KEY: &str = "Idempotency-Key";
pub const DEADLINE_HEADER: &str = "Aseman-Deadline";
pub const REQUEST_ID_HEADER: &str = "X-Request-Id";
/// How long a VMM remembers an idempotency key.
pub const IDEMPOTENCY_RETENTION_MILLIS: i64 = 24 * 60 * 60 * 1000;

/// The HTTP status every VMM answers with for a problem code.
pub trait ProblemStatus {
    fn status(self) -> u16;
}

impl ProblemStatus for ProblemCode {
    fn status(self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::Unauthenticated => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::StaleGeneration
            | Self::AlreadyExists
            | Self::InvalidTransition
            | Self::WorkloadDeleted
            | Self::IdempotencyInProgress
            | Self::OperationFinished
            | Self::StaleObservation => 409,
            Self::ResourceVersionMismatch => 412,
            Self::PayloadTooLarge => 413,
            Self::UnsupportedOperation | Self::IdempotencyKeyReused => 422,
            Self::RateLimited => 429,
            Self::BackendFailure => 502,
            Self::Unavailable => 503,
            Self::DeadlineExceeded => 504,
        }
    }
}

/// RFC 9457 problem details.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Problem {
    #[serde(rename = "type")]
    pub type_uri: String,
    pub title: String,
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    pub code: ProblemCode,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_generation: Option<u64>,
}

impl Problem {
    #[must_use]
    pub fn new(code: ProblemCode, title: &str, request_id: &str) -> Self {
        Self {
            type_uri: format!("https://aseman.dev/problems/vmm/{}", code.as_str()),
            title: title.to_owned(),
            status: code.status(),
            detail: None,
            instance: None,
            code,
            request_id: request_id.to_owned(),
            retry_after_seconds: None,
            current_generation: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkload {
    pub id: Uuid,
    pub labels: WorkloadLabels,
    pub spec: WorkloadSpec,
    pub desired: DesiredStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workload {
    pub id: Uuid,
    pub labels: WorkloadLabels,
    pub spec: WorkloadSpec,
    pub desired: DesiredStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_generation: Option<Generation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<Observation>,
    pub resource_version: String,
    pub created_at_millis: i64,
    pub updated_at_millis: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleCommand {
    pub generation: Generation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSpec {
    pub generation: Generation,
    pub spec: WorkloadSpec,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationKind {
    Signal,
    ChainTransactions,
    ChainEffects,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    pub kind: InvocationKind,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_id: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecRequest {
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_millis: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildRequest {
    pub id: Uuid,
    pub runtime: String,
    pub source: Artifact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_type: Option<String>,
    pub labels: WorkloadLabels,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildResult {
    pub artifact: Artifact,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreRequest {
    pub generation: Generation,
    pub snapshot_id: Uuid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotResult {
    pub snapshot_id: Uuid,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gas_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationRequest {
    pub program: Artifact,
    pub inputs: Vec<u64>,
    pub outputs: Vec<u64>,
    pub proof: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationResult {
    pub valid: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_level: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// An operation's result; the shape follows the operation's kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OperationResult {
    Exec(ExecResult),
    Build(BuildResult),
    Snapshot(SnapshotResult),
    Invocation(InvocationResult),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<Uuid>,
    pub kind: OperationKind,
    pub state: OperationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<Generation>,
    pub created_at_millis: i64,
    pub updated_at_millis: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_millis: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<OperationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Problem>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointList {
    pub items: Vec<Endpoint>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadEvent {
    pub sequence: u64,
    pub workload_id: Uuid,
    pub at_millis: i64,
    #[serde(rename = "type")]
    pub event_type: WorkloadEventType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<Observation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<Operation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendInfo {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub api_version: String,
    pub backend: BackendInfo,
    pub runtimes: Vec<RuntimeCapabilities>,
    pub max_request_bytes: u64,
}

impl Capabilities {
    #[must_use]
    pub fn runtime(&self, key: &str) -> Option<&RuntimeCapabilities> {
        self.runtimes.iter().find(|runtime| runtime.runtime == key)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ok,
    Degraded,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Health {
    pub status: HealthStatus,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checks: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    pub api_version: String,
    pub service: String,
    pub build: String,
    pub backend_contract: String,
}

#[cfg(test)]
mod tests;
