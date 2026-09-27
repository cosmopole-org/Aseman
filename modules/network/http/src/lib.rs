//! Hardened transport for the generated A701 public API.
//!
//! This crate is deliberately an edge. It owns HTTP/TLS, admission limits, contract
//! route lookup, authentication-header parsing, request IDs, and RFC 9457 responses.
//! It does not authenticate a session/proof, authorize an action, or execute business
//! logic. Those are one atomic responsibility of [`PublicActionService`], so this
//! transport cannot bypass A401/A402 by calling a legacy action handler directly.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aseman_contracts::guest_api::parse_proof_header;
use aseman_domain::authority::ActionClass;
use aseman_domain::identity::Proof;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Extension, Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::StreamExt;
use futures_util::stream;
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tower::ServiceExt;

pub const SESSION_HEADER: &str = "Aseman-Session";
pub const PROOF_HEADER: &str = "Aseman-Proof";
pub const REQUEST_ID_HEADER: &str = "Aseman-Request-Id";
pub const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";
pub const BRIDGE_TOKEN_HEADER: &str = "Aseman-Bridge-Token";

const OPENAPI: &str = include_str!("../../../../contracts/public/openapi.json");

/// Authentication material at the transport boundary. The service must validate it;
/// merely possessing either variant grants no authority.
#[derive(Clone)]
pub enum Authentication {
    Session(String),
    Proof(Box<Proof>),
}

/// One operation admitted by the generated public contract.
#[derive(Clone)]
pub struct PublicActionRequest {
    pub request_id: String,
    pub route: String,
    pub action: String,
    pub class: ActionClass,
    pub authentication: Authentication,
    pub idempotency_key: Option<String>,
    pub body: Vec<u8>,
}

/// An action result after authentication, authorization, and execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicActionResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// A stable application refusal translated to an RFC 9457 problem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicActionError {
    pub status: u16,
    pub reason: String,
    pub detail: String,
}

/// The one application entry point behind the public protocol.
///
/// Implementations must authenticate `authentication`, authorize `action`, and execute
/// it as one path. For mutations they must durably claim `idempotency_key` before the
/// effect and replay the first completed outcome. The transport enforces presence and
/// shape, but an in-memory edge cache is intentionally not treated as durable proof.
pub trait PublicActionService: Send + Sync {
    fn invoke(
        &self,
        request: PublicActionRequest,
    ) -> Result<PublicActionResponse, PublicActionError>;
}

/// One event already authorized for delivery to a public subscriber.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicEventFrame {
    pub event_id: String,
    pub sequence: u64,
    pub kind: String,
    /// One JSON value containing the A707 envelope and payload representation.
    pub data: String,
}

/// One bounded read from an admitted public subscription.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicEventBatch {
    Events(Vec<PublicEventFrame>),
    /// The requested sequence predates retained history. The client must resync its
    /// state instead of receiving a silently incomplete replay.
    Resync {
        oldest_sequence: Option<u64>,
        latest_sequence: Option<u64>,
    },
}

/// An admitted, creature-scoped subscription. Implementations retain the identity and
/// authorization decision so a long-lived connection never replays a signed proof.
pub trait PublicEventSubscription: Send + Sync {
    fn read(
        &self,
        after_sequence: u64,
        limit: usize,
    ) -> Result<PublicEventBatch, PublicActionError>;
}

/// Admission request for the SSE realtime edge.
#[derive(Clone)]
pub struct PublicEventRequest {
    pub request_id: String,
    pub authentication: Authentication,
    pub stream: String,
    pub token: String,
}

/// The application entry point behind the public SSE edge. Implementations must route
/// admission through A401/A402 and bind returned events to the admitted creature.
pub trait PublicEventService: Send + Sync {
    fn subscribe(
        &self,
        request: PublicEventRequest,
    ) -> Result<Arc<dyn PublicEventSubscription>, PublicActionError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicTerminalOutput {
    pub sequence: u64,
    pub channel: String,
    pub data: Vec<u8>,
}

pub trait PublicTerminalSession: Send + Sync {
    fn read(
        &self,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<PublicTerminalOutput>, PublicActionError>;
    fn write(&self, data: &[u8]) -> Result<(), PublicActionError>;
    fn resize(&self, columns: u32, rows: u32) -> Result<(), PublicActionError>;
    fn close(&self) -> Result<(), PublicActionError>;
}

#[derive(Clone)]
pub struct PublicTerminalRequest {
    pub request_id: String,
    pub authentication: Authentication,
    pub idempotency_key: String,
    pub workload_id: String,
    pub creature_id: String,
    pub vm_id: String,
}

pub trait PublicTerminalService: Send + Sync {
    fn open(
        &self,
        request: PublicTerminalRequest,
    ) -> Result<Arc<dyn PublicTerminalSession>, PublicActionError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Operation {
    action: String,
    class: ActionClass,
}

/// Routes accepted by the generated A701 contract. Unknown or withheld paths never
/// reach application code.
#[derive(Clone, Debug)]
pub struct RouteCatalog(BTreeMap<String, Operation>);

impl RouteCatalog {
    /// Parse an OpenAPI document into the narrow metadata the transport needs.
    ///
    /// # Errors
    ///
    /// Malformed JSON, a path without a POST operation, or missing Aseman metadata.
    pub fn from_openapi(document: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(document).map_err(|error| error.to_string())?;
        let paths = value
            .get("paths")
            .and_then(serde_json::Value::as_object)
            .ok_or("OpenAPI paths is not an object")?;
        let mut operations = BTreeMap::new();
        for (path, item) in paths {
            let post = item
                .get("post")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| format!("{path} has no POST operation"))?;
            let action = post
                .get("x-aseman-action")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("{path} has no x-aseman-action"))?;
            let class = post
                .get("x-aseman-class")
                .ok_or_else(|| format!("{path} has no x-aseman-class"))?;
            let class: ActionClass = serde_json::from_value(class.clone())
                .map_err(|_| format!("{path} has an invalid x-aseman-class"))?;
            operations.insert(
                path.clone(),
                Operation {
                    action: action.to_owned(),
                    class,
                },
            );
        }
        Ok(Self(operations))
    }

    fn operation(&self, path: &str) -> Option<&Operation> {
        self.0.get(path)
    }
}

impl Default for RouteCatalog {
    fn default() -> Self {
        Self::from_openapi(OPENAPI).expect("checked-in A701 OpenAPI must be valid")
    }
}

/// Bounded public listener policy. An empty CORS set refuses cross-origin requests.
#[derive(Clone, Debug)]
pub struct PublicHttpConfig {
    pub max_body_bytes: usize,
    pub max_in_flight: usize,
    pub request_timeout: Duration,
    pub requests_per_window: u32,
    pub rate_window: Duration,
    pub max_rate_subjects: usize,
    pub allowed_origins: BTreeSet<String>,
    pub drain_timeout: Duration,
}

impl Default for PublicHttpConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: 1024 * 1024,
            max_in_flight: 256,
            request_timeout: Duration::from_secs(30),
            requests_per_window: 120,
            rate_window: Duration::from_secs(60),
            max_rate_subjects: 8_192,
            allowed_origins: BTreeSet::new(),
            drain_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug)]
struct RateBucket {
    since: Instant,
    used: u32,
}

struct PublicHttpState {
    service: Arc<dyn PublicActionService>,
    events: Option<Arc<dyn PublicEventService>>,
    terminals: Option<Arc<dyn PublicTerminalService>>,
    catalog: RouteCatalog,
    config: PublicHttpConfig,
    in_flight: Arc<Semaphore>,
    rates: Mutex<HashMap<String, RateBucket>>,
}

type Shared = Arc<PublicHttpState>;

const STREAM_BATCH_EVENTS: usize = 100;
const STREAM_MAX_EVENTS: usize = 1_000;
const STREAM_POLL_INTERVAL: Duration = Duration::from_millis(250);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EventQuery {
    #[serde(default)]
    after: Option<u64>,
    #[serde(default = "default_stream_events")]
    max_events: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TerminalQuery {
    creature_id: String,
    vm_id: String,
    #[serde(default)]
    after: u64,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum TerminalClientFrame {
    Stdin { data: String },
    Resize { columns: u32, rows: u32 },
    Close,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TerminalServerFrame<'a> {
    Output {
        sequence: u64,
        channel: &'a str,
        data: String,
    },
    Error {
        status: u16,
        reason: &'a str,
        detail: &'a str,
    },
    Exit {
        code: i32,
    },
}

const fn default_stream_events() -> usize {
    STREAM_BATCH_EVENTS
}

struct EventCursor {
    subscription: Arc<dyn PublicEventSubscription>,
    buffered: VecDeque<PublicEventFrame>,
    after: u64,
    remaining: usize,
    idle_since: Instant,
    finished: bool,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

#[derive(Serialize)]
struct Problem<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    title: &'a str,
    status: u16,
    detail: &'a str,
    instance: String,
    aseman_reason: &'a str,
}

fn finish(
    status: StatusCode,
    content_type: &'static str,
    body: Vec<u8>,
    request_id: &str,
) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(value) = HeaderValue::from_str(request_id) {
        headers.insert(HeaderName::from_static("aseman-request-id"), value);
    }
    response
}

fn problem(
    status: StatusCode,
    title: &'static str,
    reason: &str,
    detail: &str,
    request_id: &str,
) -> Response {
    let value = Problem {
        kind: "about:blank",
        title,
        status: status.as_u16(),
        detail,
        instance: format!("urn:aseman:request:{request_id}"),
        aseman_reason: reason,
    };
    finish(
        status,
        "application/problem+json",
        serde_json::to_vec(&value).unwrap_or_default(),
        request_id,
    )
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
                })
        })
        .map_or_else(|| uuid::Uuid::now_v7().to_string(), str::to_owned)
}

fn authentication(headers: &HeaderMap) -> Result<Authentication, &'static str> {
    let session = headers.get(SESSION_HEADER);
    let proof = headers.get(PROOF_HEADER);
    match (session, proof) {
        (Some(_), Some(_)) => Err("ambiguous_authentication"),
        (None, None) => Err("authentication_required"),
        (Some(value), None) => value
            .to_str()
            .ok()
            .filter(|value| !value.is_empty() && value.len() <= 512)
            .map(|value| Authentication::Session(value.to_owned()))
            .ok_or("malformed_session"),
        (None, Some(value)) => {
            let value = value
                .to_str()
                .ok()
                .filter(|value| value.len() <= 16 * 1024)
                .ok_or("malformed_proof")?;
            let wire = parse_proof_header(value).map_err(|_| "malformed_proof")?;
            wire.parse()
                .map(Box::new)
                .map(Authentication::Proof)
                .map_err(|_| "malformed_proof")
        }
    }
}

fn idempotency(headers: &HeaderMap, required: bool) -> Result<Option<String>, &'static str> {
    let parsed = headers
        .get(IDEMPOTENCY_HEADER)
        .map(|value| {
            value
                .to_str()
                .ok()
                .filter(|key| {
                    (16..=128).contains(&key.len())
                        && key
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                })
                .map(str::to_owned)
                .ok_or("malformed_idempotency_key")
        })
        .transpose()?;
    if required && parsed.is_none() {
        return Err("idempotency_key_required");
    }
    Ok(parsed)
}

fn cors_origin(state: &PublicHttpState, headers: &HeaderMap) -> Result<Option<String>, ()> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(None);
    };
    let origin = origin.to_str().map_err(|_| ())?;
    state
        .config
        .allowed_origins
        .contains(origin)
        .then(|| origin.to_owned())
        .ok_or(())
        .map(Some)
}

fn add_cors(mut response: Response, origin: Option<&str>) -> Response {
    if let Some(origin) = origin.and_then(|value| HeaderValue::from_str(value).ok()) {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    response
}

fn admitted(state: &PublicHttpState, peer: Option<SocketAddr>) -> bool {
    if state.config.requests_per_window == 0 {
        return false;
    }
    let key = peer.map_or_else(|| "local".to_owned(), |peer| peer.ip().to_string());
    let mut rates = state
        .rates
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let now = Instant::now();
    if !rates.contains_key(&key) && rates.len() >= state.config.max_rate_subjects {
        rates.retain(|_, bucket| now.duration_since(bucket.since) < state.config.rate_window);
        if rates.len() >= state.config.max_rate_subjects {
            return false;
        }
    }
    let bucket = rates.entry(key).or_insert(RateBucket {
        since: now,
        used: 0,
    });
    if now.duration_since(bucket.since) >= state.config.rate_window {
        bucket.since = now;
        bucket.used = 0;
    }
    if bucket.used >= state.config.requests_per_window {
        return false;
    }
    bucket.used += 1;
    true
}

async fn action(
    State(state): State<Shared>,
    peer: Option<Extension<SocketAddr>>,
    Path(path): Path<String>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let started = Instant::now();
    let request_id = request_id(&headers);
    let origin = match cors_origin(&state, &headers) {
        Ok(origin) => origin,
        Err(()) => {
            return problem(
                StatusCode::FORBIDDEN,
                "Origin refused",
                "origin_refused",
                "the request origin is not allowed",
                &request_id,
            );
        }
    };
    let route = format!("/v1/actions/{path}");
    let Some(operation) = state.catalog.operation(&route).cloned() else {
        return add_cors(
            problem(
                StatusCode::NOT_FOUND,
                "Unknown operation",
                "route_not_found",
                "the operation is not in the public contract",
                &request_id,
            ),
            origin.as_deref(),
        );
    };
    if !admitted(&state, peer.map(|Extension(peer)| peer)) {
        let mut response = problem(
            StatusCode::TOO_MANY_REQUESTS,
            "Rate limit exceeded",
            "rate_limited",
            "retry after the admission window",
            &request_id,
        );
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        return add_cors(response, origin.as_deref());
    }
    let authentication = match authentication(&headers) {
        Ok(authentication) => authentication,
        Err(reason) => {
            return add_cors(
                problem(
                    StatusCode::UNAUTHORIZED,
                    "Authentication required",
                    reason,
                    "provide exactly one valid Aseman-Session or Aseman-Proof header",
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
    };
    let idempotency_key = match idempotency(&headers, operation.class != ActionClass::Read) {
        Ok(key) => key,
        Err(reason) => {
            return add_cors(
                problem(
                    StatusCode::BAD_REQUEST,
                    "Invalid idempotency key",
                    reason,
                    "mutations require a 16-128 character Idempotency-Key",
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
    };
    let body = match body {
        Ok(body) => body,
        Err(_) => {
            return add_cors(
                problem(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Payload too large",
                    "payload_too_large",
                    "the request body exceeds the configured limit",
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
    };
    if !serde_json::from_slice::<serde_json::Value>(&body).is_ok_and(|value| value.is_object()) {
        return add_cors(
            problem(
                StatusCode::BAD_REQUEST,
                "Invalid JSON",
                "invalid_request",
                "the request body must be a JSON object",
                &request_id,
            ),
            origin.as_deref(),
        );
    }
    let permit = match state.in_flight.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return add_cors(
                problem(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Concurrency limit reached",
                    "concurrency_limited",
                    "retry when another request completes",
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
    };
    let service = state.service.clone();
    let metric_route = operation.action.clone();
    let request = PublicActionRequest {
        request_id: request_id.clone(),
        route,
        action: operation.action,
        class: operation.class,
        authentication,
        idempotency_key,
        body: body.to_vec(),
    };
    let work = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        service.invoke(request)
    });
    let response = match tokio::time::timeout(state.config.request_timeout, work).await {
        Err(_) => problem(
            StatusCode::GATEWAY_TIMEOUT,
            "Request timed out",
            "deadline_exceeded",
            "the operation exceeded the configured duration",
            &request_id,
        ),
        Ok(Err(_)) => problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service unavailable",
            "service_unavailable",
            "the action service stopped unexpectedly",
            &request_id,
        ),
        Ok(Ok(Err(error))) => problem(
            StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            "Operation refused",
            &error.reason,
            &error.detail,
            &request_id,
        ),
        Ok(Ok(Ok(output))) => finish(
            StatusCode::from_u16(output.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            "application/json",
            output.body,
            &request_id,
        ),
    };
    let status_class = format!("{}xx", response.status().as_u16() / 100);
    aseman_observability::metrics().increment(
        "aseman_http_requests_total",
        &[
            ("service", "aseman-node"),
            ("route", metric_route.as_str()),
            ("method", "POST"),
            ("status_class", status_class.as_str()),
        ],
    );
    aseman_observability::metrics().observe(
        "aseman_http_request_duration_seconds",
        &[
            ("service", "aseman-node"),
            ("route", metric_route.as_str()),
            ("method", "POST"),
        ],
        started.elapsed().as_secs_f64(),
    );
    add_cors(response, origin.as_deref())
}

fn stream_problem(error: &PublicActionError) -> SseEvent {
    let data = serde_json::json!({
        "status": error.status,
        "reason": error.reason,
        "detail": error.detail,
    });
    SseEvent::default().event("error").data(data.to_string())
}

fn resync_event(oldest_sequence: Option<u64>, latest_sequence: Option<u64>) -> SseEvent {
    SseEvent::default().event("resync").data(
        serde_json::json!({
            "oldestSequence": oldest_sequence,
            "latestSequence": latest_sequence,
        })
        .to_string(),
    )
}

async fn next_event(
    mut cursor: EventCursor,
) -> Option<(Result<SseEvent, Infallible>, EventCursor)> {
    loop {
        if let Some(frame) = cursor.buffered.pop_front() {
            cursor.after = frame.sequence;
            cursor.remaining = cursor.remaining.saturating_sub(1);
            let event = SseEvent::default()
                // SSE Last-Event-ID is the dense A707 stream sequence. The stable A707
                // UUID remains in the JSON envelope delivered as data.
                .id(frame.sequence.to_string())
                .event(frame.kind)
                .data(frame.data);
            return Some((Ok(event), cursor));
        }
        if cursor.finished || cursor.remaining == 0 {
            return None;
        }

        let subscription = cursor.subscription.clone();
        let after = cursor.after;
        let limit = cursor.remaining.min(STREAM_BATCH_EVENTS);
        let read = tokio::task::spawn_blocking(move || subscription.read(after, limit)).await;
        match read {
            Ok(Ok(PublicEventBatch::Events(events))) if events.is_empty() => {
                if cursor.idle_since.elapsed() >= STREAM_IDLE_TIMEOUT {
                    return None;
                }
                tokio::time::sleep(STREAM_POLL_INTERVAL).await;
            }
            Ok(Ok(PublicEventBatch::Events(events))) => {
                cursor.buffered.extend(events);
                cursor.idle_since = Instant::now();
            }
            Ok(Ok(PublicEventBatch::Resync {
                oldest_sequence,
                latest_sequence,
            })) => {
                cursor.finished = true;
                return Some((Ok(resync_event(oldest_sequence, latest_sequence)), cursor));
            }
            Ok(Err(error)) => {
                cursor.finished = true;
                return Some((Ok(stream_problem(&error)), cursor));
            }
            Err(_) => {
                cursor.finished = true;
                return Some((
                    Ok(stream_problem(&PublicActionError {
                        status: 503,
                        reason: "service_unavailable".to_owned(),
                        detail: "the event service stopped unexpectedly".to_owned(),
                    })),
                    cursor,
                ));
            }
        }
    }
}

async fn events(
    State(state): State<Shared>,
    peer: Option<Extension<SocketAddr>>,
    Path(stream_name): Path<String>,
    Query(query): Query<EventQuery>,
    headers: HeaderMap,
) -> Response {
    let request_id = request_id(&headers);
    let origin = match cors_origin(&state, &headers) {
        Ok(origin) => origin,
        Err(()) => {
            return problem(
                StatusCode::FORBIDDEN,
                "Origin refused",
                "origin_refused",
                "the request origin is not allowed",
                &request_id,
            );
        }
    };
    let Some(event_service) = state.events.clone() else {
        return add_cors(
            problem(
                StatusCode::NOT_FOUND,
                "Event stream unavailable",
                "route_not_found",
                "the public event service is not composed",
                &request_id,
            ),
            origin.as_deref(),
        );
    };
    if stream_name.is_empty() || stream_name.len() > 256 {
        return add_cors(
            problem(
                StatusCode::BAD_REQUEST,
                "Invalid subscription",
                "invalid_subscription",
                "stream must be 1-256 bytes",
                &request_id,
            ),
            origin.as_deref(),
        );
    }
    let token = match headers
        .get(BRIDGE_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 4_096)
    {
        Some(token) => token.to_owned(),
        None => {
            return add_cors(
                problem(
                    StatusCode::BAD_REQUEST,
                    "Invalid subscription",
                    "invalid_subscription",
                    "Aseman-Bridge-Token must be 1-4096 bytes",
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
    };
    if query.max_events == 0 || query.max_events > STREAM_MAX_EVENTS {
        return add_cors(
            problem(
                StatusCode::BAD_REQUEST,
                "Invalid stream limit",
                "invalid_stream_limit",
                "maxEvents must be between 1 and 1000",
                &request_id,
            ),
            origin.as_deref(),
        );
    }
    if !admitted(&state, peer.map(|Extension(peer)| peer)) {
        return add_cors(
            problem(
                StatusCode::TOO_MANY_REQUESTS,
                "Rate limit exceeded",
                "rate_limited",
                "retry after the admission window",
                &request_id,
            ),
            origin.as_deref(),
        );
    }
    let authentication = match authentication(&headers) {
        Ok(authentication) => authentication,
        Err(reason) => {
            return add_cors(
                problem(
                    StatusCode::UNAUTHORIZED,
                    "Authentication required",
                    reason,
                    "provide exactly one valid Aseman-Session or Aseman-Proof header",
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
    };
    let after_header = match headers.get(HeaderName::from_static("last-event-id")) {
        None => None,
        Some(value) => match value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
        {
            Some(sequence) => Some(sequence),
            None => {
                return add_cors(
                    problem(
                        StatusCode::BAD_REQUEST,
                        "Invalid event cursor",
                        "invalid_event_cursor",
                        "Last-Event-ID must be an A707 stream sequence",
                        &request_id,
                    ),
                    origin.as_deref(),
                );
            }
        },
    };
    let permit = match state.in_flight.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return add_cors(
                problem(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Concurrency limit reached",
                    "concurrency_limited",
                    "retry when another request completes",
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
    };
    let after = after_header.or(query.after).unwrap_or(0);
    let max_events = query.max_events;
    let subscription_request_id = request_id.clone();
    let subscription = match tokio::task::spawn_blocking(move || {
        event_service.subscribe(PublicEventRequest {
            request_id: subscription_request_id,
            authentication,
            stream: stream_name,
            token,
        })
    })
    .await
    {
        Ok(Ok(subscription)) => subscription,
        Ok(Err(error)) => {
            return add_cors(
                problem(
                    StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    "Subscription refused",
                    &error.reason,
                    &error.detail,
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
        Err(_) => {
            return add_cors(
                problem(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Service unavailable",
                    "service_unavailable",
                    "the event service stopped unexpectedly",
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
    };

    let cursor = EventCursor {
        subscription,
        buffered: VecDeque::new(),
        after,
        remaining: max_events,
        idle_since: Instant::now(),
        finished: false,
        _permit: permit,
    };
    let mut response = Sse::new(stream::unfold(cursor, next_event))
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(5))
                .text("keepalive"),
        )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response.headers_mut().insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("aseman-request-id"), value);
    }
    add_cors(response, origin.as_deref())
}

async fn event_preflight(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let request_id = request_id(&headers);
    if state.events.is_none() {
        return problem(
            StatusCode::NOT_FOUND,
            "Event stream unavailable",
            "route_not_found",
            "the public event service is not composed",
            &request_id,
        );
    }
    let origin = match cors_origin(&state, &headers) {
        Ok(Some(origin)) => origin,
        _ => {
            return problem(
                StatusCode::FORBIDDEN,
                "Origin refused",
                "origin_refused",
                "the request origin is not allowed",
                &request_id,
            );
        }
    };
    let mut response = finish(
        StatusCode::NO_CONTENT,
        "application/json",
        Vec::new(),
        &request_id,
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, OPTIONS"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(
            "Aseman-Session, Aseman-Proof, Aseman-Request-Id, Aseman-Bridge-Token, Last-Event-ID",
        ),
    );
    add_cors(response, Some(&origin))
}

async fn send_terminal_frame(socket: &mut WebSocket, frame: &TerminalServerFrame<'_>) -> bool {
    let Ok(text) = serde_json::to_string(frame) else {
        return false;
    };
    socket.send(Message::Text(text)).await.is_ok()
}

async fn run_terminal(
    mut socket: WebSocket,
    session: Arc<dyn PublicTerminalSession>,
    mut after: u64,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    let mut poll = tokio::time::interval(STREAM_POLL_INTERVAL);
    loop {
        tokio::select! {
            _ = poll.tick() => {
                let reader = session.clone();
                let output = tokio::task::spawn_blocking(move || reader.read(after, 100)).await;
                match output {
                    Ok(Ok(records)) => {
                        for record in records {
                            after = after.max(record.sequence);
                            let data = base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                &record.data,
                            );
                            if !send_terminal_frame(&mut socket, &TerminalServerFrame::Output {
                                sequence: record.sequence,
                                channel: &record.channel,
                                data,
                            }).await {
                                let _ = session.close();
                                return;
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        let _ = send_terminal_frame(&mut socket, &TerminalServerFrame::Error {
                            status: error.status,
                            reason: &error.reason,
                            detail: &error.detail,
                        }).await;
                        let _ = session.close();
                        return;
                    }
                    Err(_) => {
                        let _ = send_terminal_frame(&mut socket, &TerminalServerFrame::Error {
                            status: 503,
                            reason: "service_unavailable",
                            detail: "the terminal service stopped unexpectedly",
                        }).await;
                        let _ = session.close();
                        return;
                    }
                }
            }
            incoming = socket.next() => {
                let Some(Ok(message)) = incoming else {
                    let _ = session.close();
                    return;
                };
                let result = match message {
                    Message::Text(text) => match serde_json::from_str::<TerminalClientFrame>(&text) {
                        Ok(TerminalClientFrame::Stdin { data }) => {
                            base64::Engine::decode(
                                &base64::engine::general_purpose::STANDARD,
                                data,
                            )
                            .map_err(|_| PublicActionError {
                                status: 400,
                                reason: "invalid_terminal_frame".to_owned(),
                                detail: "stdin data is not base64".to_owned(),
                            })
                            .and_then(|bytes| session.write(&bytes))
                        }
                        Ok(TerminalClientFrame::Resize { columns, rows }) => {
                            session.resize(columns, rows)
                        }
                        Ok(TerminalClientFrame::Close) => {
                            let _ = session.close();
                            let _ = send_terminal_frame(
                                &mut socket,
                                &TerminalServerFrame::Exit { code: 0 },
                            ).await;
                            return;
                        }
                        Err(_) => Err(PublicActionError {
                            status: 400,
                            reason: "invalid_terminal_frame".to_owned(),
                            detail: "terminal text frame is malformed".to_owned(),
                        }),
                    },
                    Message::Close(_) => {
                        let _ = session.close();
                        return;
                    }
                    Message::Ping(data) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            let _ = session.close();
                            return;
                        }
                        Ok(())
                    }
                    Message::Pong(_) => Ok(()),
                    Message::Binary(_) => Err(PublicActionError {
                        status: 400,
                        reason: "invalid_terminal_frame".to_owned(),
                        detail: "terminal input uses JSON text frames".to_owned(),
                    }),
                };
                if let Err(error) = result
                    && !send_terminal_frame(&mut socket, &TerminalServerFrame::Error {
                        status: error.status,
                        reason: &error.reason,
                        detail: &error.detail,
                    }).await
                {
                    let _ = session.close();
                    return;
                }
            }
        }
    }
}

async fn terminal(
    State(state): State<Shared>,
    peer: Option<Extension<SocketAddr>>,
    Path(workload_id): Path<String>,
    Query(query): Query<TerminalQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let request_id = request_id(&headers);
    let origin = match cors_origin(&state, &headers) {
        Ok(origin) => origin,
        Err(()) => {
            return problem(
                StatusCode::FORBIDDEN,
                "Origin refused",
                "origin_refused",
                "the request origin is not allowed",
                &request_id,
            );
        }
    };
    let Some(terminals) = state.terminals.clone() else {
        return add_cors(
            problem(
                StatusCode::NOT_IMPLEMENTED,
                "Terminal unavailable",
                "unsupported_operation",
                "no terminal provider is composed",
                &request_id,
            ),
            origin.as_deref(),
        );
    };
    if !admitted(&state, peer.map(|Extension(peer)| peer)) {
        return add_cors(
            problem(
                StatusCode::TOO_MANY_REQUESTS,
                "Rate limit exceeded",
                "rate_limited",
                "retry after the admission window",
                &request_id,
            ),
            origin.as_deref(),
        );
    }
    let authentication = match authentication(&headers) {
        Ok(authentication) => authentication,
        Err(reason) => {
            return add_cors(
                problem(
                    StatusCode::UNAUTHORIZED,
                    "Authentication required",
                    reason,
                    "provide exactly one valid Aseman-Session or Aseman-Proof header",
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
    };
    let idempotency_key = match idempotency(&headers, true) {
        Ok(Some(key)) => key,
        _ => {
            return add_cors(
                problem(
                    StatusCode::BAD_REQUEST,
                    "Invalid idempotency key",
                    "idempotency_key_required",
                    "terminal admission requires a 16-128 character Idempotency-Key",
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
    };
    if workload_id.is_empty()
        || query.creature_id.is_empty()
        || query.vm_id.is_empty()
        || workload_id.len() > 128
    {
        return add_cors(
            problem(
                StatusCode::BAD_REQUEST,
                "Invalid terminal target",
                "invalid_terminal_target",
                "workloadId, creatureId, and vmId are required",
                &request_id,
            ),
            origin.as_deref(),
        );
    }
    let permit = match state.in_flight.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return add_cors(
                problem(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Concurrency limit reached",
                    "concurrency_limited",
                    "retry when another request completes",
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
    };
    let open = PublicTerminalRequest {
        request_id: request_id.clone(),
        authentication,
        idempotency_key,
        workload_id,
        creature_id: query.creature_id,
        vm_id: query.vm_id,
    };
    let session = match tokio::task::spawn_blocking(move || terminals.open(open)).await {
        Ok(Ok(session)) => session,
        Ok(Err(error)) => {
            return add_cors(
                problem(
                    StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    "Terminal refused",
                    &error.reason,
                    &error.detail,
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
        Err(_) => {
            return add_cors(
                problem(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Terminal unavailable",
                    "service_unavailable",
                    "the terminal service stopped unexpectedly",
                    &request_id,
                ),
                origin.as_deref(),
            );
        }
    };
    add_cors(
        upgrade
            .protocols(["aseman.terminal.v1"])
            .on_upgrade(move |socket| run_terminal(socket, session, query.after, permit)),
        origin.as_deref(),
    )
}

async fn preflight(
    State(state): State<Shared>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    let request_id = request_id(&headers);
    if state
        .catalog
        .operation(&format!("/v1/actions/{path}"))
        .is_none()
    {
        return problem(
            StatusCode::NOT_FOUND,
            "Unknown operation",
            "route_not_found",
            "the operation is not in the public contract",
            &request_id,
        );
    }
    let origin = match cors_origin(&state, &headers) {
        Ok(Some(origin)) => origin,
        _ => {
            return problem(
                StatusCode::FORBIDDEN,
                "Origin refused",
                "origin_refused",
                "the request origin is not allowed",
                &request_id,
            );
        }
    };
    let mut response = finish(
        StatusCode::NO_CONTENT,
        "application/json",
        Vec::new(),
        &request_id,
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, OPTIONS"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(
            "Content-Type, Aseman-Session, Aseman-Proof, Aseman-Request-Id, Idempotency-Key",
        ),
    );
    add_cors(response, Some(&origin))
}

/// Construct the public router from the checked-in generated contract.
pub fn router(service: Arc<dyn PublicActionService>, config: PublicHttpConfig) -> Router {
    router_with_streams(service, None, None, config)
}

/// Construct the public router with the creature-scoped A707 SSE edge enabled.
pub fn router_with_events(
    service: Arc<dyn PublicActionService>,
    event_service: Option<Arc<dyn PublicEventService>>,
    config: PublicHttpConfig,
) -> Router {
    router_with_streams(service, event_service, None, config)
}

/// Construct the public router with both durable SSE and WebSocket terminal edges.
pub fn router_with_streams(
    service: Arc<dyn PublicActionService>,
    event_service: Option<Arc<dyn PublicEventService>>,
    terminal_service: Option<Arc<dyn PublicTerminalService>>,
    config: PublicHttpConfig,
) -> Router {
    let max_body_bytes = config.max_body_bytes;
    let max_in_flight = config.max_in_flight.max(1);
    let state = Arc::new(PublicHttpState {
        service,
        events: event_service,
        terminals: terminal_service,
        catalog: RouteCatalog::default(),
        in_flight: Arc::new(Semaphore::new(max_in_flight)),
        rates: Mutex::new(HashMap::new()),
        config,
    });
    Router::new()
        .route("/v1/actions/*path", post(action).options(preflight))
        .route("/v1/events/:stream", get(events).options(event_preflight))
        .route("/v1/terminals/:workload_id", get(terminal))
        .layer(DefaultBodyLimit::max(max_body_bytes))
        .with_state(state)
}

/// Serve TLS until shutdown, then stop accepting connections and drain each open
/// HTTP/1 connection for at most `config.drain_timeout`.
///
/// # Errors
///
/// Invalid TLS material or a listener failure during shutdown setup.
pub async fn serve(
    listener: TcpListener,
    certificate_chain: Vec<CertificateDer<'static>>,
    private_key: PrivateKeyDer<'static>,
    service: Arc<dyn PublicActionService>,
    config: PublicHttpConfig,
    shutdown: impl Future<Output = ()>,
) -> Result<(), String> {
    serve_with_events(
        listener,
        certificate_chain,
        private_key,
        service,
        None,
        config,
        shutdown,
    )
    .await
}

/// Serve TLS with the A707 SSE edge composed beside the A701 unary actions.
///
/// # Errors
///
/// Invalid TLS material or a listener failure during shutdown setup.
pub async fn serve_with_events(
    listener: TcpListener,
    certificate_chain: Vec<CertificateDer<'static>>,
    private_key: PrivateKeyDer<'static>,
    service: Arc<dyn PublicActionService>,
    events: Option<Arc<dyn PublicEventService>>,
    config: PublicHttpConfig,
    shutdown: impl Future<Output = ()>,
) -> Result<(), String> {
    serve_with_streams(
        listener,
        certificate_chain,
        private_key,
        service,
        PublicStreamServices {
            events,
            terminals: None,
        },
        config,
        shutdown,
    )
    .await
}

/// Optional streaming services hosted beside the unary public API.
#[derive(Clone, Default)]
pub struct PublicStreamServices {
    pub events: Option<Arc<dyn PublicEventService>>,
    pub terminals: Option<Arc<dyn PublicTerminalService>>,
}

/// Serve TLS with durable SSE and WebSocket terminal streams composed beside A701.
pub async fn serve_with_streams(
    listener: TcpListener,
    certificate_chain: Vec<CertificateDer<'static>>,
    private_key: PrivateKeyDer<'static>,
    service: Arc<dyn PublicActionService>,
    streams: PublicStreamServices,
    config: PublicHttpConfig,
    shutdown: impl Future<Output = ()>,
) -> Result<(), String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_no_client_auth()
        .with_single_cert(certificate_chain, private_key)
        .map_err(|error| error.to_string())?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let app = router_with_streams(service, streams.events, streams.terminals, config.clone());
    let (drain_tx, drain_rx) = watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                let (socket, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(_) => continue,
                };
                let acceptor = acceptor.clone();
                let app = app.clone();
                let mut drain = drain_rx.clone();
                tasks.spawn(async move {
                    let Ok(stream) = acceptor.accept(socket).await else {
                        return;
                    };
                    let service = hyper::service::service_fn(
                        move |mut request: hyper::Request<hyper::body::Incoming>| {
                            request.extensions_mut().insert(peer);
                            app.clone().oneshot(request.map(Body::new))
                        },
                    );
                    let connection = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .with_upgrades();
                    tokio::pin!(connection);
                    tokio::select! {
                        _ = &mut connection => {}
                        changed = drain.changed() => {
                            if changed.is_ok() && *drain.borrow() {
                                connection.as_mut().graceful_shutdown();
                                let _ = (&mut connection).await;
                            }
                        }
                    }
                });
            }
        }
    }
    let _ = drain_tx.send(true);
    let drained = async { while tasks.join_next().await.is_some() {} };
    if tokio::time::timeout(config.drain_timeout, drained)
        .await
        .is_err()
    {
        tasks.abort_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests;
