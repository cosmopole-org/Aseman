//! The narrow authenticated A603 HTTP boundary.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use aseman_contracts::vmm_agent::{AgentAnswer, SignedGrant, grant_message};
use aseman_domain::agent::AgentOperation;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use base64::Engine;
use hyper_util::rt::TokioIo;
use ring::signature::{ED25519, UnparsedPublicKey};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tower::ServiceExt;

use crate::host::{Agent, Refusal};

pub struct GrantVerifier {
    epoch: u64,
    public_key: Vec<u8>,
}

impl GrantVerifier {
    #[must_use]
    pub fn new(epoch: u64, public_key: Vec<u8>) -> Self {
        Self { epoch, public_key }
    }

    pub fn verify(&self, signed: &SignedGrant) -> Result<(), &'static str> {
        if signed.key_epoch != self.epoch {
            return Err("the grant is not signed by the active VMM epoch");
        }
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&signed.signature)
            .map_err(|_| "the grant signature is malformed")?;
        let message = grant_message(signed.key_epoch, &signed.grant)
            .map_err(|_| "the grant cannot be encoded")?;
        UnparsedPublicKey::new(&ED25519, &self.public_key)
            .verify(&message, &signature)
            .map_err(|_| "the grant is not signed by the VMM")
    }
}

pub struct AgentHttpState {
    pub agent: Arc<Agent>,
    pub verifier: GrantVerifier,
}

#[derive(Clone, Debug, Serialize)]
struct Problem<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    title: &'static str,
    status: u16,
    detail: &'a str,
}

fn problem(status: StatusCode, detail: &str) -> Response {
    (
        status,
        Json(Problem {
            kind: "about:blank",
            title: "Agent request refused",
            status: status.as_u16(),
            detail,
        }),
    )
        .into_response()
}

fn operation(value: &str) -> Option<AgentOperation> {
    Some(match value {
        "create" => AgentOperation::Create,
        "start" => AgentOperation::Start,
        "pause" => AgentOperation::Pause,
        "resume" => AgentOperation::Resume,
        "state" => AgentOperation::State,
        "stop" => AgentOperation::Stop,
        "delete" => AgentOperation::Delete,
        _ => return None,
    })
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

async fn invoke(
    State(state): State<Arc<AgentHttpState>>,
    Extension(_client): Extension<String>,
    Path((allocation, operation_name)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let Some(operation) = operation(&operation_name) else {
        return problem(StatusCode::NOT_FOUND, "unknown agent operation");
    };
    let signed: SignedGrant = match serde_json::from_slice(&body) {
        Ok(signed) => signed,
        Err(_) => return problem(StatusCode::BAD_REQUEST, "invalid signed grant"),
    };
    if let Err(reason) = state.verifier.verify(&signed) {
        return problem(StatusCode::UNAUTHORIZED, reason);
    }
    let result = if operation == AgentOperation::Create {
        state.agent.create(&signed.grant, &allocation, now_millis())
    } else {
        state
            .agent
            .operate(&signed.grant, &allocation, operation, now_millis())
    };
    match result {
        Ok(machine_state) => Json(AgentAnswer {
            allocation,
            operation,
            state: machine_state,
        })
        .into_response(),
        Err(Refusal::Rule(reason)) => problem(StatusCode::FORBIDDEN, &reason.to_string()),
        Err(Refusal::Host(reason)) => problem(StatusCode::SERVICE_UNAVAILABLE, &reason),
    }
}

async fn health(State(state): State<Arc<AgentHttpState>>) -> Response {
    match state.agent.capability() {
        Ok(()) => (StatusCode::OK, "ready").into_response(),
        Err(reason) => problem(StatusCode::SERVICE_UNAVAILABLE, &reason.to_string()),
    }
}

pub fn router(state: Arc<AgentHttpState>, max_body_bytes: usize) -> Router {
    Router::new()
        .route("/v1/allocations/:allocation/:operation", post(invoke))
        .route("/health/ready", get(health))
        .layer(DefaultBodyLimit::max(max_body_bytes))
        .with_state(state)
}

pub struct ServerTls {
    pub certificate_chain: Vec<CertificateDer<'static>>,
    pub private_key: PrivateKeyDer<'static>,
    pub client_roots: Vec<CertificateDer<'static>>,
    /// SHA-256 leaf-certificate fingerprint to an audit-facing client name.
    pub clients: BTreeMap<[u8; 32], String>,
}

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

pub async fn serve(
    listener: TcpListener,
    tls: ServerTls,
    state: Arc<AgentHttpState>,
    max_body_bytes: usize,
    shutdown: impl Future<Output = ()>,
) -> Result<(), String> {
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config(&tls)?));
    let clients = Arc::new(tls.clients);
    let app = router(state, max_body_bytes);
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
            let client = stream
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|chain| chain.first())
                .map(|leaf| Sha256::digest(leaf.as_ref()).into())
                .and_then(|fingerprint: [u8; 32]| clients.get(&fingerprint).cloned());
            let Some(client) = client else {
                return;
            };
            let service = hyper::service::service_fn(
                move |mut request: hyper::Request<hyper::body::Incoming>| {
                    request.extensions_mut().insert(client.clone());
                    app.clone().oneshot(request.map(Body::new))
                },
            );
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_domain::agent::Grant;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn grant() -> Grant {
        Grant {
            allocation: "alloc-one".to_owned(),
            profile: "small".to_owned(),
            expires_at_millis: i64::MAX,
            operations: [AgentOperation::State].into_iter().collect(),
        }
    }

    #[test]
    fn signed_grants_bind_epoch_and_every_grant_field() {
        let seed = [7_u8; 32];
        let pair = Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
        let mut signed = SignedGrant {
            key_epoch: 3,
            grant: grant(),
            signature: String::new(),
        };
        let signature = pair.sign(&grant_message(signed.key_epoch, &signed.grant).unwrap());
        signed.signature =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.as_ref());
        let verifier = GrantVerifier::new(3, pair.public_key().as_ref().to_vec());
        assert_eq!(verifier.verify(&signed), Ok(()));
        signed.grant.allocation = "alloc-two".to_owned();
        assert_eq!(
            verifier.verify(&signed),
            Err("the grant is not signed by the VMM")
        );
    }
}
