//! The node's guest API listener.

use std::future::Future;
use std::sync::Arc;

use aseman_application::guest_call::GuestRequest;
use aseman_application::identity::IdentityFailure;
use aseman_contracts::guest_api::{PROOF_HEADER, parse_proof_header};
use aseman_domain::identity::Proof;
use aseman_ports::PortError;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tower::ServiceExt;

/// Bodies above this are refused.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// The node's side: authenticate the proof and serve the request as the workload
/// (`ServeGuestCall`). `body` is the exact request body the proof signs.
pub trait GuestApi: Send + Sync {
    /// # Errors
    ///
    /// As `ServeGuestCall::execute`.
    fn serve(
        &self,
        proof: &Proof,
        body: &[u8],
        request: GuestRequest<'_>,
    ) -> Result<Vec<u8>, IdentityFailure>;
}

type Shared = Arc<dyn GuestApi>;

fn reply(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn problem(status: StatusCode, code: &str, detail: &str) -> Response {
    reply(
        status,
        "application/problem+json",
        serde_json::json!({"code": code, "detail": detail})
            .to_string()
            .into_bytes(),
    )
}

fn failure(error: &IdentityFailure) -> Response {
    match error {
        IdentityFailure::Rejected(code) => problem(StatusCode::UNAUTHORIZED, code.code(), ""),
        IdentityFailure::Refused(reason) => problem(StatusCode::FORBIDDEN, "refused", reason),
        IdentityFailure::Unavailable(PortError::NotFound) => {
            problem(StatusCode::NOT_FOUND, "not_found", "")
        }
        IdentityFailure::Unavailable(PortError::Denied(reason)) => {
            problem(StatusCode::FORBIDDEN, "denied", reason)
        }
        IdentityFailure::Unavailable(PortError::Unsupported(reason)) => {
            problem(StatusCode::UNPROCESSABLE_ENTITY, "unsupported", reason)
        }
        IdentityFailure::Unavailable(PortError::Failed(detail)) => {
            problem(StatusCode::UNPROCESSABLE_ENTITY, "failed", detail)
        }
        IdentityFailure::Unavailable(_) => {
            problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "")
        }
    }
}

/// Parse the proof header (A401 steps 1-3); a refusal is its A401 code.
fn proof(headers: &HeaderMap) -> Result<Proof, &'static str> {
    let value = headers
        .get(PROOF_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 16 * 1024)
        .ok_or("malformed")?;
    let wire = parse_proof_header(value).map_err(|_| "malformed")?;
    wire.parse().map_err(|error| error.code())
}

async fn blocking(
    work: impl FnOnce() -> Result<Vec<u8>, IdentityFailure> + Send + 'static,
    content_type: &'static str,
) -> Response {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(body)) => reply(StatusCode::OK, content_type, body),
        Ok(Err(error)) => failure(&error),
        Err(_) => problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable", ""),
    }
}

async fn call(
    State(api): State<Shared>,
    Path(op): Path<String>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let Ok(body) = body else {
        return problem(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", "");
    };
    let proof = match proof(&headers) {
        Ok(proof) => proof,
        Err(code) => return problem(StatusCode::UNAUTHORIZED, code, ""),
    };
    blocking(
        move || {
            let input = std::str::from_utf8(&body)
                .map_err(|_| IdentityFailure::Refused("the input is not UTF-8 JSON"))?;
            api.serve(&proof, &body, GuestRequest::Call { op: &op, input })
        },
        "application/json",
    )
    .await
}

async fn artifact(
    State(api): State<Shared>,
    Path(digest): Path<String>,
    headers: HeaderMap,
) -> Response {
    let proof = match proof(&headers) {
        Ok(proof) => proof,
        Err(code) => return problem(StatusCode::UNAUTHORIZED, code, ""),
    };
    blocking(
        move || api.serve(&proof, &[], GuestRequest::Artifact { digest: &digest }),
        "application/octet-stream",
    )
    .await
}

/// The guest API routes.
pub fn router(api: Shared) -> Router {
    Router::new()
        .route("/guest/v1/calls/:op", post(call))
        .route("/guest/v1/artifacts/:digest", get(artifact))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(api)
}

/// Serve the guest API with TLS (server authentication; each request authenticates
/// its workload with a proof) until `shutdown` completes.
///
/// # Errors
///
/// Invalid TLS material.
pub async fn serve(
    listener: TcpListener,
    certificate_chain: Vec<CertificateDer<'static>>,
    private_key: PrivateKeyDer<'static>,
    api: Shared,
    shutdown: impl Future<Output = ()>,
) -> Result<(), String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_no_client_auth()
        .with_single_cert(certificate_chain, private_key)
        .map_err(|error| error.to_string())?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let app = router(api);
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
        let app = app.clone();
        tokio::spawn(async move {
            let Ok(stream) = acceptor.accept(socket).await else {
                return;
            };
            let service = hyper::service::service_fn(
                move |request: hyper::Request<hyper::body::Incoming>| {
                    app.clone().oneshot(request.map(Body::new))
                },
            );
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}
