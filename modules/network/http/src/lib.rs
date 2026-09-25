//! Hardened transport for the generated A701 public API.
//!
//! This crate is deliberately an edge. It owns HTTP/TLS, admission limits, contract
//! route lookup, authentication-header parsing, request IDs, and RFC 9457 responses.
//! It does not authenticate a session/proof, authorize an action, or execute business
//! logic. Those are one atomic responsibility of [`PublicActionService`], so this
//! transport cannot bypass A401/A402 by calling a legacy action handler directly.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tower::ServiceExt;

pub const SESSION_HEADER: &str = "Aseman-Session";
pub const PROOF_HEADER: &str = "Aseman-Proof";
pub const REQUEST_ID_HEADER: &str = "Aseman-Request-Id";
pub const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";

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
    catalog: RouteCatalog,
    config: PublicHttpConfig,
    in_flight: Arc<Semaphore>,
    rates: Mutex<HashMap<String, RateBucket>>,
}

type Shared = Arc<PublicHttpState>;

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
    add_cors(response, origin.as_deref())
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
    let max_body_bytes = config.max_body_bytes;
    let max_in_flight = config.max_in_flight.max(1);
    let state = Arc::new(PublicHttpState {
        service,
        catalog: RouteCatalog::default(),
        in_flight: Arc::new(Semaphore::new(max_in_flight)),
        rates: Mutex::new(HashMap::new()),
        config,
    });
    Router::new()
        .route("/v1/actions/*path", post(action).options(preflight))
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
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_no_client_auth()
        .with_single_cert(certificate_chain, private_key)
        .map_err(|error| error.to_string())?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let app = router(service, config.clone());
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
                        .serve_connection(TokioIo::new(stream), service);
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
