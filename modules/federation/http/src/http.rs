//! Bounded federation HTTP adapter over the destination-side application use case.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use aseman_application::federation::{Refusal, ServeFederatedRequest, Served};
use aseman_application::identity::{AuthenticateProof, IdentityFailure, VerifierPolicy};
use aseman_contracts::guest_api::parse_proof_header;
use aseman_domain::Uuid;
use aseman_domain::federation::Envelope;
use aseman_domain::identity::{
    FreshnessPolicy, Proof, RotationPolicy, SignatureContext, SubjectKind,
};
use aseman_ports::federation::{Directory, EnvelopeGuard};
use aseman_ports::{
    ClockPort, IdentityVerifier, KeyDirectory, PolicyDecisionPort, PortError, ReplayGuard,
};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tower::ServiceExt;

pub const PROOF_HEADER: &str = "Aseman-Proof";

#[derive(Clone, Copy, Debug)]
pub struct FederationHttpConfig {
    pub max_body_bytes: usize,
    pub drain_timeout: Duration,
}

impl Default for FederationHttpConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: 1024 * 1024,
            drain_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    envelope: Envelope,
    payload_base64: String,
}

/// Canonical signed federation response. `signature` covers the JSON encoding of the
/// other four fields in declaration order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SignedFederationResponse {
    pub request_id: Uuid,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub signature: String,
}

#[derive(Serialize)]
struct UnsignedResponse<'a> {
    request_id: Uuid,
    outcome: &'a str,
    answer: &'a Option<String>,
    reason: &'a Option<String>,
}

/// Executes the already authenticated and destination-authorized operation.
pub trait FederationExecutor: Send + Sync {
    fn execute(&self, envelope: &Envelope, payload: &[u8]) -> Result<String, PortError>;
}

/// Signs response bytes with this node's current federation response key.
pub trait FederationResponseSigner: Send + Sync {
    fn sign(&self, response: &[u8]) -> Result<String, PortError>;
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum FederationHttpError {
    #[error("malformed federation request: {0}")]
    Malformed(&'static str),
    #[error("federation authentication failed: {0}")]
    Authentication(String),
    #[error("federation dependency unavailable: {0}")]
    Unavailable(String),
}

impl FederationHttpError {
    fn status(&self) -> StatusCode {
        match self {
            Self::Malformed(_) => StatusCode::BAD_REQUEST,
            Self::Authentication(_) => StatusCode::UNAUTHORIZED,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Malformed(_) => "invalid_federation_request",
            Self::Authentication(_) => "federation_authentication_failed",
            Self::Unavailable(_) => "federation_unavailable",
        }
    }
}

/// Complete inbound federation composition. Every dependency is a canonical port;
/// this module has no route to legacy node action handlers.
pub struct FederationService {
    pub keys: Arc<dyn KeyDirectory>,
    pub replay: Arc<dyn ReplayGuard>,
    pub verifier: Arc<dyn IdentityVerifier>,
    pub directory: Arc<dyn Directory>,
    pub guard: Arc<dyn EnvelopeGuard>,
    pub policy: Arc<dyn PolicyDecisionPort>,
    pub clock: Arc<dyn ClockPort>,
    pub executor: Arc<dyn FederationExecutor>,
    pub response_signer: Arc<dyn FederationResponseSigner>,
    pub node_id: Uuid,
    pub audience: String,
}

impl FederationService {
    fn signed(
        &self,
        request_id: Uuid,
        outcome: &'static str,
        answer: Option<String>,
        reason: Option<String>,
    ) -> Result<SignedFederationResponse, FederationHttpError> {
        let unsigned = UnsignedResponse {
            request_id,
            outcome,
            answer: &answer,
            reason: &reason,
        };
        let bytes = serde_json::to_vec(&unsigned)
            .map_err(|error| FederationHttpError::Unavailable(error.to_string()))?;
        let signature = self
            .response_signer
            .sign(&bytes)
            .map_err(|error| FederationHttpError::Unavailable(error.to_string()))?;
        Ok(SignedFederationResponse {
            request_id,
            outcome: outcome.to_owned(),
            answer,
            reason,
            signature,
        })
    }
}

pub trait FederationHttpHandler: Send + Sync {
    fn invoke(
        &self,
        proof: &Proof,
        envelope: &Envelope,
        payload: &[u8],
    ) -> Result<SignedFederationResponse, FederationHttpError>;
}

impl FederationHttpHandler for FederationService {
    fn invoke(
        &self,
        proof: &Proof,
        envelope: &Envelope,
        payload: &[u8],
    ) -> Result<SignedFederationResponse, FederationHttpError> {
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(payload)));
        if envelope.payload_digest != digest {
            return Err(FederationHttpError::Malformed("payload digest mismatch"));
        }
        if proof.context != SignatureContext::Request
            || proof.subject.kind != SubjectKind::Node
            || proof.subject.id != envelope.source_node
            || proof.request_id != envelope.request_id.to_string()
            || proof.action != envelope.action
            || proof.resource != envelope.target
        {
            return Err(FederationHttpError::Authentication(
                "proof does not bind the envelope".to_owned(),
            ));
        }
        AuthenticateProof {
            keys: self.keys.as_ref(),
            replay: self.replay.as_ref(),
            verifier: self.verifier.as_ref(),
            clock: self.clock.as_ref(),
        }
        .execute(
            proof,
            payload,
            &VerifierPolicy {
                audience: self.audience.clone(),
                freshness: FreshnessPolicy {
                    max_clock_skew_millis: 30_000,
                    max_lifetime_millis: 60_000,
                },
                rotation: RotationPolicy::DEFAULT,
            },
        )
        .map_err(|error| match error {
            IdentityFailure::Rejected(error) => {
                FederationHttpError::Authentication(error.code().to_owned())
            }
            other => FederationHttpError::Unavailable(other.to_string()),
        })?;

        let served = ServeFederatedRequest {
            directory: self.directory.as_ref(),
            guard: self.guard.as_ref(),
            policy: self.policy.as_ref(),
            clock: self.clock.as_ref(),
            node_id: self.node_id,
        }
        .serve(envelope, |accepted| {
            self.executor.execute(accepted, payload)
        })
        .map_err(|error| FederationHttpError::Unavailable(error.to_string()))?;

        match served {
            Served::Executed(answer) => {
                self.signed(envelope.request_id, "executed", Some(answer), None)
            }
            Served::Replayed(answer) => {
                self.signed(envelope.request_id, "replayed", Some(answer), None)
            }
            Served::Refused(reason) => self.signed(
                envelope.request_id,
                "refused",
                None,
                Some(refusal_code(&reason).to_owned()),
            ),
        }
    }
}

fn refusal_code(refusal: &Refusal) -> &'static str {
    match refusal {
        Refusal::Envelope(_) => "invalid_envelope",
        Refusal::UnknownPeer => "unknown_peer",
        Refusal::Denied(_) => "destination_denied",
        Refusal::UnknownSubject => "unknown_subject",
        Refusal::UnknownTarget => "unknown_target",
    }
}

#[derive(Serialize)]
struct Problem<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    title: &'static str,
    status: u16,
    detail: &'a str,
    aseman_reason: &'static str,
}

async fn receive(
    State(handler): State<Arc<dyn FederationHttpHandler>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let result = (|| {
        if headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| !value.starts_with("application/json"))
        {
            return Err(FederationHttpError::Malformed(
                "content type must be application/json",
            ));
        }
        let header = headers
            .get(PROOF_HEADER)
            .and_then(|value| value.to_str().ok())
            .ok_or(FederationHttpError::Authentication(
                "Aseman-Proof is required".to_owned(),
            ))?;
        let proof_wire = parse_proof_header(header)
            .map_err(|_| FederationHttpError::Authentication("malformed proof".to_owned()))?;
        let proof = proof_wire
            .parse()
            .map_err(|_| FederationHttpError::Authentication("malformed proof".to_owned()))?;
        let wire: WireRequest = serde_json::from_slice(&body)
            .map_err(|_| FederationHttpError::Malformed("invalid JSON body"))?;
        let payload = URL_SAFE_NO_PAD
            .decode(wire.payload_base64)
            .map_err(|_| FederationHttpError::Malformed("invalid payload encoding"))?;
        handler.invoke(&proof, &wire.envelope, &payload)
    })();
    match result {
        Ok(response) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::to_vec(&response).unwrap_or_default(),
        )
            .into_response(),
        Err(error) => {
            let detail = error.to_string();
            (
                error.status(),
                [(header::CONTENT_TYPE, "application/problem+json")],
                serde_json::to_vec(&Problem {
                    kind: "about:blank",
                    title: "Federation request refused",
                    status: error.status().as_u16(),
                    detail: &detail,
                    aseman_reason: error.code(),
                })
                .unwrap_or_default(),
            )
                .into_response()
        }
    }
}

pub fn router(handler: Arc<dyn FederationHttpHandler>, config: FederationHttpConfig) -> Router {
    Router::new()
        .route("/v1/federation/envelopes", post(receive))
        .layer(DefaultBodyLimit::max(config.max_body_bytes))
        .with_state(handler)
}

/// Server identity and the federation CA whose client certificates are accepted.
pub struct FederationServerTls {
    pub certificate_chain: Vec<CertificateDer<'static>>,
    pub private_key: PrivateKeyDer<'static>,
    pub client_roots: Vec<CertificateDer<'static>>,
}

/// Build the mandatory-mutual-TLS federation server configuration.
pub fn tls_config(tls: &FederationServerTls) -> Result<rustls::ServerConfig, String> {
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

/// Serve the federation endpoint with mandatory client certificates, then drain open
/// HTTP/1 connections for the configured bound when shutdown begins.
pub async fn serve(
    listener: TcpListener,
    tls: FederationServerTls,
    handler: Arc<dyn FederationHttpHandler>,
    config: FederationHttpConfig,
    shutdown: impl Future<Output = ()>,
) -> Result<(), String> {
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config(&tls)?));
    let app = router(handler, config);
    let (drain_tx, drain_rx) = watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                let (socket, _) = match accepted {
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
                        move |request: hyper::Request<hyper::body::Incoming>| {
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
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;

    #[derive(Default)]
    struct Recording(AtomicUsize);

    impl FederationHttpHandler for Recording {
        fn invoke(
            &self,
            _proof: &Proof,
            _envelope: &Envelope,
            _payload: &[u8],
        ) -> Result<SignedFederationResponse, FederationHttpError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            unreachable!("malformed transport input must never reach the service")
        }
    }

    #[tokio::test]
    async fn malformed_or_unsigned_requests_never_reach_application_code() {
        let handler = Arc::new(Recording::default());
        let app = router(handler.clone(), FederationHttpConfig::default());
        let unsigned = Request::builder()
            .method("POST")
            .uri("/v1/federation/envelopes")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(unsigned).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let wrong_type = Request::builder()
            .method("POST")
            .uri("/v1/federation/envelopes")
            .header(PROOF_HEADER, "bad")
            .body(Body::from("{}"))
            .unwrap();
        assert_eq!(
            app.oneshot(wrong_type).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(handler.0.load(Ordering::SeqCst), 0);
    }
}
