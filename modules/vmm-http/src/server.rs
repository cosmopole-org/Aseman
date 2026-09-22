//! The A501 server: mutual TLS, idempotency keys, deadlines, request IDs, RFC 9457
//! problems, cursor pagination, and SSE over the VMM service use cases.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use aseman_application::vmm::{OperationTarget, VmmError, VmmService};
use aseman_contracts::vmm::{
    BuildRequest, CreateWorkload, DEADLINE_HEADER, EndpointList, ExecRequest, Health, HealthStatus,
    HttpRequest, HttpResponse, IDEMPOTENCY_KEY, Invocation, InvocationKind,
    LifecycleCommand as WireCommand, Page, ProblemCode, ProblemStatus, REQUEST_ID_HEADER,
    RestoreRequest, UpdateSpec, VerificationRequest, VerificationResult, Version,
};
use aseman_domain::vmm::WorkloadOperation;
use aseman_domain::{Generation, ObservedWorkloadState, OperationId, OperationState, WorkloadId};
use aseman_ports::ClockPort;
use aseman_ports::vmm::{
    IdempotencyClaim, IdempotencyStore, LifecycleCommand, NewWorkload, OperationFilter,
    ReplayableResponse, VmmBackend, VmmEventLog, VmmOperationStore, VmmWorkloadStore,
    WorkloadFilter,
};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Extension, FromRequest, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::stream::{self, Stream};
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tower::ServiceExt;

use crate::wire;

/// A claim that never completed is taken over after this long.
const CLAIM_TTL_MILLIS: i64 = 5 * 60 * 1000;
const STREAM_POLL: Duration = Duration::from_millis(250);
const STREAM_BATCH: usize = 200;

/// The calling node, established from its client certificate.
#[derive(Clone, Debug)]
pub struct Owner(pub String);

/// What the server serves from.
pub struct VmmHttpState {
    pub workloads: Arc<dyn VmmWorkloadStore>,
    pub operations: Arc<dyn VmmOperationStore>,
    pub events: Arc<dyn VmmEventLog>,
    pub idempotency: Arc<dyn IdempotencyStore>,
    pub backend: Arc<dyn VmmBackend>,
    pub clock: Arc<dyn ClockPort>,
    pub max_request_bytes: usize,
}

impl VmmHttpState {
    #[must_use]
    pub fn service(&self) -> VmmService<'_> {
        VmmService {
            workloads: &*self.workloads,
            operations: &*self.operations,
            events: &*self.events,
            backend: &*self.backend,
            clock: &*self.clock,
        }
    }
}

type Shared = Arc<VmmHttpState>;

/// Per-request values from the common headers.
struct Context {
    request_id: String,
    deadline_millis: Option<i64>,
    if_match: Option<u64>,
}

/// A response before its common headers are added.
struct Reply {
    status: StatusCode,
    body: Vec<u8>,
    content_type: &'static str,
    location: Option<String>,
    etag: Option<String>,
}

impl Reply {
    fn json<T: Serialize>(status: StatusCode, value: &T) -> Self {
        Self {
            status,
            body: serde_json::to_vec(value).unwrap_or_default(),
            content_type: "application/json",
            location: None,
            etag: None,
        }
    }

    fn problem(code: ProblemCode, detail: &str, request_id: &str) -> Self {
        let problem = wire::problem(code, detail, request_id);
        Self {
            status: StatusCode::from_u16(code.status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            body: serde_json::to_vec(&problem).unwrap_or_default(),
            content_type: "application/problem+json",
            location: None,
            etag: None,
        }
    }

    fn error(error: &VmmError, request_id: &str) -> Self {
        let mut problem = wire::problem(error.failure, &error.detail, request_id);
        problem.current_generation = error.current_generation.map(Generation::get);
        Self {
            status: StatusCode::from_u16(error.failure.status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            body: serde_json::to_vec(&problem).unwrap_or_default(),
            content_type: "application/problem+json",
            location: None,
            etag: None,
        }
    }

    fn accepted(operation: &aseman_domain::vmm::OperationRecord, request_id: &str) -> Self {
        Self {
            location: Some(format!("/v1/operations/{}", operation.id)),
            ..Self::json(
                StatusCode::ACCEPTED,
                &wire::operation(operation, request_id),
            )
        }
    }

    fn finish(self, request_id: &str) -> Response {
        let mut response = Response::new(Body::from(self.body));
        *response.status_mut() = self.status;
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(self.content_type),
        );
        if let Ok(value) = HeaderValue::from_str(request_id) {
            headers.insert(REQUEST_ID_HEADER, value);
        }
        if let Some(location) = self
            .location
            .and_then(|value| HeaderValue::from_str(&value).ok())
        {
            headers.insert(header::LOCATION, location);
        }
        if let Some(etag) = self
            .etag
            .and_then(|value| HeaderValue::from_str(&format!("\"{value}\"")).ok())
        {
            headers.insert(header::ETAG, etag);
        }
        response
    }
}

fn context(headers: &HeaderMap) -> Result<Context, Reply> {
    let request_id = headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .map_or_else(|| uuid::Uuid::now_v7().to_string(), str::to_owned);
    let deadline_millis = match headers.get(DEADLINE_HEADER) {
        None => None,
        Some(value) => Some(
            value
                .to_str()
                .ok()
                .and_then(|text| text.parse::<i64>().ok())
                .ok_or_else(|| {
                    Reply::problem(ProblemCode::InvalidRequest, "invalid deadline", &request_id)
                })?,
        ),
    };
    let if_match = match headers.get(header::IF_MATCH) {
        None => None,
        Some(value) => Some(
            value
                .to_str()
                .ok()
                .map(|text| text.trim().trim_matches('"'))
                .and_then(|text| text.parse::<u64>().ok())
                .ok_or_else(|| {
                    Reply::problem(ProblemCode::InvalidRequest, "invalid If-Match", &request_id)
                })?,
        ),
    };
    Ok(Context {
        request_id,
        deadline_millis,
        if_match,
    })
}

fn parse_id<T>(text: &str, wrap: fn(uuid::Uuid) -> T, request_id: &str) -> Result<T, Reply> {
    uuid::Uuid::parse_str(text)
        .ok()
        .filter(|id| id.hyphenated().to_string() == text)
        .map(wrap)
        .ok_or_else(|| {
            Reply::problem(
                ProblemCode::InvalidRequest,
                "invalid identifier",
                request_id,
            )
        })
}

fn decode<T: for<'de> Deserialize<'de>>(body: &[u8], request_id: &str) -> Result<T, Reply> {
    serde_json::from_slice(body).map_err(|error| {
        Reply::problem(ProblemCode::InvalidRequest, &error.to_string(), request_id)
    })
}

fn valid_key(key: &str) -> bool {
    (16..=128).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Run a read handler on a blocking thread.
async fn read(
    headers: HeaderMap,
    handler: impl FnOnce(&Context) -> Reply + Send + 'static,
) -> Response {
    let context = match context(&headers) {
        Ok(context) => context,
        Err(reply) => return reply.finish(&uuid::Uuid::now_v7().to_string()),
    };
    let request_id = context.request_id.clone();
    tokio::task::spawn_blocking(move || handler(&context))
        .await
        .unwrap_or_else(|_| Reply::problem(ProblemCode::Unavailable, "", &request_id))
        .finish(&request_id)
}

/// A mutation's method, target, headers, and body (or why the body was refused).
pub struct Mutation {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequest<S> for Mutation {
    type Rejection = Infallible;

    async fn from_request(request: axum::extract::Request, state: &S) -> Result<Self, Infallible> {
        let method = request.method().clone();
        let uri = request.uri().clone();
        let headers = request.headers().clone();
        let body = Bytes::from_request(request, state).await;
        Ok(Self {
            method,
            uri,
            headers,
            body,
        })
    }
}

/// Run a mutation once per idempotency key (A501): a replay returns the first
/// response, another body under the same key is refused.
async fn mutate(
    state: Shared,
    owner: Owner,
    mutation: Mutation,
    handler: impl FnOnce(&VmmHttpState, &str, &Context, &[u8]) -> Reply + Send + 'static,
) -> Response {
    let Mutation {
        method,
        uri,
        headers,
        body,
    } = mutation;
    let context = match context(&headers) {
        Ok(context) => context,
        Err(reply) => return reply.finish(&uuid::Uuid::now_v7().to_string()),
    };
    let request_id = context.request_id.clone();
    let body = match body {
        Ok(body) => body,
        Err(_) => {
            return Reply::problem(ProblemCode::PayloadTooLarge, "", &request_id)
                .finish(&request_id);
        }
    };
    let Some(key) = headers
        .get(IDEMPOTENCY_KEY)
        .and_then(|value| value.to_str().ok())
        .filter(|key| valid_key(key))
        .map(str::to_owned)
    else {
        return Reply::problem(
            ProblemCode::InvalidRequest,
            "Idempotency-Key is required",
            &request_id,
        )
        .finish(&request_id);
    };
    let mut digest = Sha256::new();
    digest.update(method.as_str());
    digest.update([0]);
    digest.update(uri.path());
    digest.update([0]);
    digest.update(uri.query().unwrap_or(""));
    digest.update([0]);
    digest.update(&body);
    let digest: [u8; 32] = digest.finalize().into();
    tokio::task::spawn_blocking(move || {
        let now = state.clock.unix_millis();
        match state
            .idempotency
            .claim(&owner.0, &key, digest, now, CLAIM_TTL_MILLIS)
        {
            Ok(IdempotencyClaim::Completed(stored)) => Reply {
                status: StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK),
                body: stored.body,
                content_type: if stored.content_type == "application/problem+json" {
                    "application/problem+json"
                } else if stored.content_type == "application/json" {
                    "application/json"
                } else {
                    "application/octet-stream"
                },
                location: stored.location,
                etag: None,
            },
            Ok(IdempotencyClaim::InProgress) => {
                Reply::problem(ProblemCode::IdempotencyInProgress, "", &context.request_id)
            }
            Ok(IdempotencyClaim::Mismatch) => {
                Reply::problem(ProblemCode::IdempotencyKeyReused, "", &context.request_id)
            }
            Ok(IdempotencyClaim::Claimed) => {
                let reply = handler(&state, &owner.0, &context, &body);
                // A retryable failure may be retried under the same key; anything
                // else is the answer for good.
                let outcome = if reply.status.is_server_error() {
                    state.idempotency.release(&owner.0, &key)
                } else {
                    state.idempotency.complete(
                        &owner.0,
                        &key,
                        &ReplayableResponse {
                            status: reply.status.as_u16(),
                            body: reply.body.clone(),
                            content_type: reply.content_type.to_owned(),
                            location: reply.location.clone(),
                        },
                    )
                };
                match outcome {
                    Ok(()) => reply,
                    Err(_) => Reply::problem(ProblemCode::Unavailable, "", &context.request_id),
                }
            }
            Err(_) => Reply::problem(ProblemCode::Unavailable, "", &context.request_id),
        }
    })
    .await
    .unwrap_or_else(|_| Reply::problem(ProblemCode::Unavailable, "", &request_id))
    .finish(&request_id)
}

fn operation_or(
    result: Result<aseman_domain::vmm::OperationRecord, VmmError>,
    context: &Context,
) -> Reply {
    match result {
        Ok(operation) => Reply::accepted(&operation, &context.request_id),
        Err(error) => Reply::error(&error, &context.request_id),
    }
}

macro_rules! attempt {
    ($expression:expr) => {
        match $expression {
            Ok(value) => value,
            Err(reply) => return reply,
        }
    };
}

async fn capabilities(State(state): State<Shared>, headers: HeaderMap) -> Response {
    read(headers, move |context| match state.backend.describe() {
        Ok(description) => Reply::json(
            StatusCode::OK,
            &wire::capabilities(description, state.max_request_bytes as u64),
        ),
        Err(error) => Reply::error(&error.into(), &context.request_id),
    })
    .await
}

async fn create_workload(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    mutation: Mutation,
) -> Response {
    mutate(state, owner, mutation, |state, owner, context, body| {
        let request: CreateWorkload = attempt!(decode(body, &context.request_id));
        if request.spec.bootstrap.credential.is_none() {
            return Reply::problem(
                ProblemCode::InvalidRequest,
                "the bootstrap credential is required",
                &context.request_id,
            );
        }
        let workload = NewWorkload {
            id: WorkloadId::from_uuid(request.id),
            labels: request.labels,
            spec: request.spec,
            desired: request.desired,
        };
        operation_or(
            state
                .service()
                .create(owner, &workload, context.deadline_millis),
            context,
        )
    })
    .await
}

#[derive(Deserialize)]
struct ListQuery {
    cursor: Option<String>,
    limit: Option<usize>,
    creature_id: Option<uuid::Uuid>,
    observed_state: Option<ObservedWorkloadState>,
    workload_id: Option<uuid::Uuid>,
    state: Option<OperationState>,
    generation: Option<u64>,
    follow: Option<bool>,
    since_millis: Option<i64>,
}

fn limit(query: &ListQuery) -> usize {
    query.limit.unwrap_or(100).clamp(1, 500)
}

async fn list_workloads(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    read(headers, move |context| {
        let filter = WorkloadFilter {
            creature_id: query.creature_id,
            observed_state: query.observed_state,
        };
        match state
            .service()
            .workloads(&owner.0, &filter, query.cursor.as_deref(), limit(&query))
        {
            Ok(page) => Reply::json(
                StatusCode::OK,
                &Page {
                    items: page.items.iter().map(wire::workload).collect(),
                    next_cursor: page.next_cursor,
                },
            ),
            Err(error) => Reply::error(&error, &context.request_id),
        }
    })
    .await
}

async fn get_workload(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    read(headers, move |context| {
        let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
        match state.service().workload(&owner.0, id) {
            Ok(record) => Reply {
                etag: Some(record.resource_version.to_string()),
                ..Reply::json(StatusCode::OK, &wire::workload(&record))
            },
            Err(error) => Reply::error(&error, &context.request_id),
        }
    })
    .await
}

async fn lifecycle(
    command: LifecycleCommand,
    state: Shared,
    owner: Owner,
    id: String,
    mutation: Mutation,
) -> Response {
    mutate(
        state,
        owner,
        mutation,
        move |state, owner, context, body| {
            let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
            let request: WireCommand = attempt!(decode(body, &context.request_id));
            operation_or(
                state.service().command(
                    owner,
                    id,
                    command,
                    request.generation,
                    context.if_match,
                    context.deadline_millis,
                ),
                context,
            )
        },
    )
    .await
}

async fn start_workload(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    mutation: Mutation,
) -> Response {
    lifecycle(LifecycleCommand::Start, state, owner, id, mutation).await
}

async fn stop_workload(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    mutation: Mutation,
) -> Response {
    lifecycle(LifecycleCommand::Stop, state, owner, id, mutation).await
}

async fn pause_workload(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    mutation: Mutation,
) -> Response {
    lifecycle(LifecycleCommand::Pause, state, owner, id, mutation).await
}

async fn resume_workload(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    mutation: Mutation,
) -> Response {
    lifecycle(LifecycleCommand::Resume, state, owner, id, mutation).await
}

async fn delete_workload(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
    mutation: Mutation,
) -> Response {
    mutate(state, owner, mutation, move |state, owner, context, _| {
        let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
        let Some(generation) = query
            .generation
            .and_then(|value| Generation::from_stored(value).ok())
        else {
            return Reply::problem(
                ProblemCode::InvalidRequest,
                "generation is required",
                &context.request_id,
            );
        };
        operation_or(
            state.service().command(
                owner,
                id,
                LifecycleCommand::Delete,
                generation,
                context.if_match,
                context.deadline_millis,
            ),
            context,
        )
    })
    .await
}

async fn update_spec(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    mutation: Mutation,
) -> Response {
    mutate(
        state,
        owner,
        mutation,
        move |state, owner, context, body| {
            let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
            let request: UpdateSpec = attempt!(decode(body, &context.request_id));
            operation_or(
                state.service().update_spec(
                    owner,
                    id,
                    &request.spec,
                    request.generation,
                    context.if_match,
                    context.deadline_millis,
                ),
                context,
            )
        },
    )
    .await
}

async fn restore_workload(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    mutation: Mutation,
) -> Response {
    mutate(
        state,
        owner,
        mutation,
        move |state, owner, context, body| {
            let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
            let request: RestoreRequest = attempt!(decode(body, &context.request_id));
            let text = serde_json::to_string(&request).unwrap_or_default();
            operation_or(
                state.service().restore(
                    owner,
                    id,
                    request.generation,
                    text,
                    context.if_match,
                    context.deadline_millis,
                ),
                context,
            )
        },
    )
    .await
}

/// A data-plane operation on a workload, recorded for the executor.
fn submit(
    state: &VmmHttpState,
    owner: &str,
    context: &Context,
    target: OperationTarget,
    operation: WorkloadOperation,
    request: String,
) -> Reply {
    operation_or(
        state
            .service()
            .submit(owner, &target, operation, request, context.deadline_millis),
        context,
    )
}

async fn snapshot_workload(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    mutation: Mutation,
) -> Response {
    mutate(state, owner, mutation, move |state, owner, context, _| {
        let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
        submit(
            state,
            owner,
            context,
            OperationTarget::Workload(id),
            WorkloadOperation::Snapshot,
            "{}".to_owned(),
        )
    })
    .await
}

async fn invoke_workload(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    mutation: Mutation,
) -> Response {
    mutate(
        state,
        owner,
        mutation,
        move |state, owner, context, body| {
            let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
            let request: Invocation = attempt!(decode(body, &context.request_id));
            let operation = match request.kind {
                InvocationKind::Signal => WorkloadOperation::Invoke,
                InvocationKind::ChainTransactions | InvocationKind::ChainEffects => {
                    WorkloadOperation::InvokeChain
                }
            };
            let text = serde_json::to_string(&request).unwrap_or_default();
            submit(
                state,
                owner,
                context,
                OperationTarget::Workload(id),
                operation,
                text,
            )
        },
    )
    .await
}

async fn exec_workload(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    mutation: Mutation,
) -> Response {
    mutate(
        state,
        owner,
        mutation,
        move |state, owner, context, body| {
            let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
            let request: ExecRequest = attempt!(decode(body, &context.request_id));
            let text = serde_json::to_string(&request).unwrap_or_default();
            submit(
                state,
                owner,
                context,
                OperationTarget::Workload(id),
                WorkloadOperation::Exec,
                text,
            )
        },
    )
    .await
}

async fn forward_http(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    mutation: Mutation,
) -> Response {
    mutate(
        state,
        owner,
        mutation,
        move |state, owner, context, body| {
            let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
            let request: HttpRequest = attempt!(decode(body, &context.request_id));
            let text = serde_json::to_string(&request).unwrap_or_default();
            match state.service().forward_http(owner, id, &text) {
                // The backend's answer must itself be an A501 `HttpResponse`.
                Ok(answer) => match serde_json::from_str::<HttpResponse>(&answer) {
                    Ok(response) => Reply::json(StatusCode::OK, &response),
                    Err(_) => Reply::problem(
                        ProblemCode::BackendFailure,
                        "the backend answered outside the contract",
                        &context.request_id,
                    ),
                },
                Err(error) => Reply::error(&error, &context.request_id),
            }
        },
    )
    .await
}

async fn put_file(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path((id, path)): Path<(String, String)>,
    mutation: Mutation,
) -> Response {
    mutate(
        state,
        owner,
        mutation,
        move |state, owner, context, body| {
            let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
            match state.service().put_file(owner, id, &path, body) {
                Ok(()) => Reply {
                    status: StatusCode::NO_CONTENT,
                    body: Vec::new(),
                    content_type: "application/octet-stream",
                    location: None,
                    etag: None,
                },
                Err(error) => Reply::error(&error, &context.request_id),
            }
        },
    )
    .await
}

async fn get_file(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path((id, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    read(headers, move |context| {
        let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
        match state.service().get_file(&owner.0, id, &path) {
            Ok(bytes) => Reply {
                status: StatusCode::OK,
                body: bytes,
                content_type: "application/octet-stream",
                location: None,
                etag: None,
            },
            Err(error) => Reply::error(&error, &context.request_id),
        }
    })
    .await
}

async fn endpoints(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    read(headers, move |context| {
        let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
        match state.service().endpoints(&owner.0, id) {
            Ok(items) => Reply::json(StatusCode::OK, &EndpointList { items }),
            Err(error) => Reply::error(&error, &context.request_id),
        }
    })
    .await
}

async fn usage(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    read(headers, move |context| {
        let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
        match state.service().usage(&owner.0, id) {
            Ok(usage) => Reply::json(StatusCode::OK, &usage),
            Err(error) => Reply::error(&error, &context.request_id),
        }
    })
    .await
}

async fn terminal(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    read(headers, move |context| {
        let id = attempt!(parse_id(&id, WorkloadId::from_uuid, &context.request_id));
        let service = state.service();
        let record = match service.workload(&owner.0, id) {
            Ok(record) => record,
            Err(error) => return Reply::error(&error, &context.request_id),
        };
        let supported = state.backend.describe().ok().and_then(|description| {
            description
                .runtimes
                .into_iter()
                .find(|runtime| runtime.runtime == record.spec.runtime)
        });
        match supported {
            Some(runtime) if runtime.terminal => Reply::problem(
                ProblemCode::Unavailable,
                "terminal attachment is not served by this VMM version",
                &context.request_id,
            ),
            _ => Reply::problem(ProblemCode::UnsupportedOperation, "", &context.request_id),
        }
    })
    .await
}

async fn create_build(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    mutation: Mutation,
) -> Response {
    mutate(state, owner, mutation, |state, owner, context, body| {
        let request: BuildRequest = attempt!(decode(body, &context.request_id));
        let text = serde_json::to_string(&request).unwrap_or_default();
        submit(
            state,
            owner,
            context,
            OperationTarget::Runtime(request.runtime),
            WorkloadOperation::Build,
            text,
        )
    })
    .await
}

async fn verify_execution(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(runtime): Path<String>,
    mutation: Mutation,
) -> Response {
    mutate(state, owner, mutation, move |state, _, context, body| {
        let request: VerificationRequest = attempt!(decode(body, &context.request_id));
        let text = serde_json::to_string(&request).unwrap_or_default();
        match state.service().verify(&runtime, &text) {
            Ok(answer) => match serde_json::from_str::<VerificationResult>(&answer) {
                Ok(result) => Reply::json(StatusCode::OK, &result),
                Err(_) => Reply::problem(
                    ProblemCode::BackendFailure,
                    "the backend answered outside the contract",
                    &context.request_id,
                ),
            },
            Err(error) => Reply::error(&error, &context.request_id),
        }
    })
    .await
}

async fn list_operations(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    read(headers, move |context| {
        let filter = OperationFilter {
            workload_id: query.workload_id.map(WorkloadId::from_uuid),
            state: query.state,
        };
        match state
            .service()
            .operations(&owner.0, &filter, query.cursor.as_deref(), limit(&query))
        {
            Ok(page) => Reply::json(
                StatusCode::OK,
                &Page {
                    items: page
                        .items
                        .iter()
                        .map(|operation| wire::operation(operation, &context.request_id))
                        .collect(),
                    next_cursor: page.next_cursor,
                },
            ),
            Err(error) => Reply::error(&error, &context.request_id),
        }
    })
    .await
}

async fn get_operation(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    read(headers, move |context| {
        let id = attempt!(parse_id(&id, OperationId::from_uuid, &context.request_id));
        match state.service().operation(&owner.0, id) {
            Ok(operation) => Reply::json(
                StatusCode::OK,
                &wire::operation(&operation, &context.request_id),
            ),
            Err(error) => Reply::error(&error, &context.request_id),
        }
    })
    .await
}

async fn cancel_operation(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    mutation: Mutation,
) -> Response {
    mutate(state, owner, mutation, move |state, owner, context, _| {
        let id = attempt!(parse_id(&id, OperationId::from_uuid, &context.request_id));
        match state.service().cancel(owner, id) {
            Ok(operation) => Reply::json(
                StatusCode::OK,
                &wire::operation(&operation, &context.request_id),
            ),
            Err(error) => Reply::error(&error, &context.request_id),
        }
    })
    .await
}

fn last_event_id(headers: &HeaderMap) -> u64 {
    headers
        .get("Last-Event-ID")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

type SseStream = std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

/// Events of `owner` (optionally one workload's) after `after`, as SSE. With
/// `follow`, the stream polls for new events; `resync` ends it.
fn event_stream(
    state: Shared,
    owner: String,
    workload: Option<WorkloadId>,
    after: u64,
    follow: bool,
) -> SseStream {
    Box::pin(stream::unfold(
        (state, owner, after, Vec::<Event>::new(), false),
        move |(state, owner, mut after, mut pending, mut done)| async move {
            loop {
                if let Some(event) = pending.pop() {
                    return Some((Ok(event), (state, owner, after, pending, done)));
                }
                if done {
                    return None;
                }
                let reader = state.clone();
                let who = owner.clone();
                let batch = tokio::task::spawn_blocking(move || {
                    let batch = reader
                        .events
                        .events_after(&who, after, workload, STREAM_BATCH)?;
                    let events: Vec<(u64, String, String)> = batch
                        .events
                        .iter()
                        .map(|record| {
                            let operation = record.operation.and_then(|id| {
                                reader
                                    .operations
                                    .operation(&who, id)
                                    .ok()
                                    .flatten()
                                    .map(|operation| wire::operation(&operation, ""))
                            });
                            let event = wire::event(record, operation);
                            let kind = serde_json::to_value(event.event_type)
                                .ok()
                                .and_then(|value| value.as_str().map(str::to_owned))
                                .unwrap_or_default();
                            (
                                record.sequence,
                                kind,
                                serde_json::to_string(&event).unwrap_or_default(),
                            )
                        })
                        .collect();
                    Ok::<_, aseman_ports::PortError>((events, batch.resync))
                })
                .await;
                match batch {
                    Ok(Ok((events, resync))) => {
                        if resync {
                            pending.push(Event::default().event("resync").data("{}"));
                            done = true;
                            continue;
                        }
                        let empty = events.is_empty();
                        for (sequence, kind, data) in events.into_iter().rev() {
                            after = after.max(sequence);
                            pending.push(
                                Event::default()
                                    .id(sequence.to_string())
                                    .event(kind)
                                    .data(data),
                            );
                        }
                        if empty {
                            if !follow {
                                return None;
                            }
                            tokio::time::sleep(STREAM_POLL).await;
                        }
                    }
                    _ => return None,
                }
            }
        },
    ))
}

async fn stream_events(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    let after = last_event_id(&headers);
    Sse::new(event_stream(
        state,
        owner.0,
        None,
        after,
        query.follow.unwrap_or(true),
    ))
    .keep_alive(KeepAlive::default())
    .into_response()
}

async fn stream_workload_events(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    let request_id = uuid::Uuid::now_v7().to_string();
    let id = match parse_id(&id, WorkloadId::from_uuid, &request_id) {
        Ok(id) => id,
        Err(reply) => return reply.finish(&request_id),
    };
    let after = last_event_id(&headers);
    Sse::new(event_stream(
        state,
        owner.0,
        Some(id),
        after,
        query.follow.unwrap_or(true),
    ))
    .keep_alive(KeepAlive::default())
    .into_response()
}

async fn stream_logs(
    State(state): State<Shared>,
    Extension(owner): Extension<Owner>,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    let request_id = uuid::Uuid::now_v7().to_string();
    let id = match parse_id(&id, WorkloadId::from_uuid, &request_id) {
        Ok(id) => id,
        Err(reply) => return reply.finish(&request_id),
    };
    // Refuse an unknown workload before streaming.
    let check = state.clone();
    let who = owner.0.clone();
    match tokio::task::spawn_blocking(move || check.service().workload(&who, id)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => return Reply::error(&error, &request_id).finish(&request_id),
        Err(_) => {
            return Reply::problem(ProblemCode::Unavailable, "", &request_id).finish(&request_id);
        }
    }
    let follow = query.follow.unwrap_or(false);
    let since = query.since_millis;
    let after = last_event_id(&headers);
    let stream: SseStream = Box::pin(stream::unfold(
        (state, owner.0, after, Vec::<Event>::new()),
        move |(state, owner, mut after, mut pending)| async move {
            loop {
                if let Some(event) = pending.pop() {
                    return Some((Ok(event), (state, owner, after, pending)));
                }
                let reader = state.clone();
                let who = owner.clone();
                let records = tokio::task::spawn_blocking(move || {
                    reader.service().logs(&who, id, after, STREAM_BATCH)
                })
                .await;
                match records {
                    Ok(Ok(records)) => {
                        let empty = records.is_empty();
                        for record in records.into_iter().rev() {
                            after = after.max(record.sequence);
                            if since.is_some_and(|since| record.at_millis < since) {
                                continue;
                            }
                            pending.push(
                                Event::default()
                                    .id(record.sequence.to_string())
                                    .event("log")
                                    .data(serde_json::to_string(&record).unwrap_or_default()),
                            );
                        }
                        if empty {
                            if !follow {
                                return None;
                            }
                            tokio::time::sleep(STREAM_POLL).await;
                        }
                    }
                    _ => return None,
                }
            }
        },
    ));
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn live(headers: HeaderMap) -> Response {
    read(headers, |_| {
        Reply::json(
            StatusCode::OK,
            &Health {
                status: HealthStatus::Ok,
                checks: BTreeMap::new(),
            },
        )
    })
    .await
}

async fn ready(State(state): State<Shared>, headers: HeaderMap) -> Response {
    read(headers, move |_| {
        let backend = state.backend.describe().is_ok();
        let store = state.workloads.all_workloads(None, 1).is_ok();
        let check = |ok: bool| if ok { "ok" } else { "unavailable" }.to_owned();
        let health = Health {
            status: if backend && store {
                HealthStatus::Ok
            } else {
                HealthStatus::Unavailable
            },
            checks: BTreeMap::from([
                ("backend".to_owned(), check(backend)),
                ("store".to_owned(), check(store)),
            ]),
        };
        Reply::json(
            if backend && store {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            },
            &health,
        )
    })
    .await
}

async fn version(State(state): State<Shared>, headers: HeaderMap) -> Response {
    read(headers, move |_| {
        let contract = state
            .backend
            .describe()
            .map(|description| description.contract)
            .unwrap_or_default();
        Reply::json(
            StatusCode::OK,
            &Version {
                api_version: aseman_contracts::vmm::API_VERSION.to_owned(),
                service: "aseman-vmm".to_owned(),
                build: env!("CARGO_PKG_VERSION").to_owned(),
                backend_contract: contract,
            },
        )
    })
    .await
}

/// The unauthenticated probes, for a plain listener the orchestrator can reach.
pub fn health_router(state: Shared) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .with_state(state)
}

/// Every A501 route. Requests must carry an [`Owner`] extension, which [`serve`]
/// sets from the client certificate.
pub fn router(state: Shared) -> Router {
    let limit = state.max_request_bytes;
    Router::new()
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/workloads", post(create_workload).get(list_workloads))
        .route(
            "/v1/workloads/:workload_id",
            get(get_workload).delete(delete_workload),
        )
        .route(
            "/v1/workloads/:workload_id/spec",
            axum::routing::put(update_spec),
        )
        .route("/v1/workloads/:workload_id/start", post(start_workload))
        .route("/v1/workloads/:workload_id/stop", post(stop_workload))
        .route("/v1/workloads/:workload_id/pause", post(pause_workload))
        .route("/v1/workloads/:workload_id/resume", post(resume_workload))
        .route("/v1/workloads/:workload_id/restore", post(restore_workload))
        .route(
            "/v1/workloads/:workload_id/snapshots",
            post(snapshot_workload),
        )
        .route(
            "/v1/workloads/:workload_id/invocations",
            post(invoke_workload),
        )
        .route("/v1/workloads/:workload_id/exec", post(exec_workload))
        .route("/v1/workloads/:workload_id/http", post(forward_http))
        .route(
            "/v1/workloads/:workload_id/files/*path",
            axum::routing::put(put_file).get(get_file),
        )
        .route("/v1/workloads/:workload_id/endpoints", get(endpoints))
        .route("/v1/workloads/:workload_id/logs", get(stream_logs))
        .route(
            "/v1/workloads/:workload_id/events",
            get(stream_workload_events),
        )
        .route("/v1/workloads/:workload_id/usage", get(usage))
        .route("/v1/workloads/:workload_id/terminal", get(terminal))
        .route("/v1/events", get(stream_events))
        .route("/v1/builds", post(create_build))
        .route(
            "/v1/runtimes/:runtime/verifications",
            post(verify_execution),
        )
        .route("/v1/operations", get(list_operations))
        .route("/v1/operations/:operation_id", get(get_operation))
        .route(
            "/v1/operations/:operation_id/cancel",
            post(cancel_operation),
        )
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/version", get(version))
        .layer(DefaultBodyLimit::max(limit))
        .with_state(state)
}

/// The server's TLS material and the clients it serves.
pub struct ServerTls {
    pub certificate_chain: Vec<CertificateDer<'static>>,
    pub private_key: PrivateKeyDer<'static>,
    /// Roots that client certificates must chain to.
    pub client_roots: Vec<CertificateDer<'static>>,
    /// SHA-256 of each admitted client's leaf certificate, and the node it is.
    pub clients: BTreeMap<[u8; 32], String>,
}

/// The TLS configuration: TLS 1.2 or 1.3, and a client certificate from
/// `client_roots` is required.
///
/// # Errors
///
/// Invalid certificates or keys.
pub fn tls_config(tls: &ServerTls) -> Result<rustls::ServerConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    for root in &tls.client_roots {
        roots.add(root.clone()).map_err(|error| error.to_string())?;
    }
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .map_err(|error| error.to_string())?;
    rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_client_cert_verifier(verifier)
        .with_single_cert(tls.certificate_chain.clone(), tls.private_key.clone_key())
        .map_err(|error| error.to_string())
}

/// Serve A501 on `listener` until `shutdown` completes. A connection whose client
/// certificate is not admitted is closed before any request is read.
///
/// # Errors
///
/// Invalid TLS material.
pub async fn serve(
    listener: TcpListener,
    tls: ServerTls,
    state: Shared,
    shutdown: impl Future<Output = ()>,
) -> Result<(), String> {
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config(&tls)?));
    let clients = Arc::new(tls.clients);
    let app = router(state);
    tokio::pin!(shutdown);
    loop {
        let (socket, _) = tokio::select! {
            () = &mut shutdown => return Ok(()),
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(_) => continue,
            },
        };
        let acceptor = acceptor.clone();
        let clients = clients.clone();
        let app = app.clone();
        tokio::spawn(async move {
            let Ok(stream) = acceptor.accept(socket).await else {
                return;
            };
            let owner = stream
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|chain| chain.first())
                .map(|leaf| Sha256::digest(leaf.as_ref()).into())
                .and_then(|fingerprint: [u8; 32]| clients.get(&fingerprint).cloned());
            let Some(owner) = owner else {
                return;
            };
            let service = hyper::service::service_fn(
                move |mut request: hyper::Request<hyper::body::Incoming>| {
                    request.extensions_mut().insert(Owner(owner.clone()));
                    app.clone().oneshot(request.map(Body::new))
                },
            );
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades()
                .await;
        });
    }
}
