//! Outbound federation HTTP with mutual TLS, bounded retries, and a circuit breaker.
//!
//! A retry reuses the same A705 request ID and nonce. The destination's durable
//! envelope guard therefore replays the first answer instead of executing twice.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aseman_contracts::identity::{PublicKey, SignedFields, body_digest, sign_ed25519};
use aseman_domain::Uuid;
use aseman_domain::federation::{Envelope, FederationReply, NodeDescriptor};
use aseman_domain::identity::{CredentialWindow, SignatureContext, Subject, SubjectKind};
use aseman_ports::PortError;
use aseman_ports::federation::Transport;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::StatusCode;
use reqwest::blocking::Client;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::Serialize;
use thiserror::Error;

use crate::http::{PROOF_HEADER, SignedFederationResponse};

/// PEM material for authenticating both peers at the transport boundary.
pub struct FederationTls {
    pub server_roots_pem: Vec<u8>,
    /// Client certificate chain followed by its private key.
    pub identity_pem: Vec<u8>,
}

/// The canonical node key used for A401 federation request proofs.
pub struct FederationNodeCredential {
    pub node_id: Uuid,
    pub key_epoch: u32,
    pub signing_key_pkcs8: Vec<u8>,
}

/// Bounded failure policy. All durations include connection and response time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FederationClientConfig {
    pub deadline: Duration,
    pub attempts: u32,
    pub initial_backoff: Duration,
    pub maximum_backoff: Duration,
    pub circuit_failure_threshold: u32,
    pub circuit_open_for: Duration,
}

impl Default for FederationClientConfig {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(30),
            attempts: 4,
            initial_backoff: Duration::from_millis(100),
            maximum_backoff: Duration::from_secs(2),
            circuit_failure_threshold: 5,
            circuit_open_for: Duration::from_secs(30),
        }
    }
}

/// Produces the A401 proof that binds the exact A705 envelope and payload.
pub trait FederationProofSigner: Send + Sync {
    fn proof_header(&self, envelope: &Envelope, payload: &[u8]) -> Result<String, String>;
}

/// Verifies the destination signature using the trusted descriptor/key directory.
pub trait FederationResponseVerifier: Send + Sync {
    fn verify(&self, message: &[u8], signature: &str) -> Result<(), String>;
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum FederationClientError {
    #[error("invalid federation client configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("the federation peer circuit is open")]
    CircuitOpen,
    #[error("could not sign the federation request: {0}")]
    Signing(String),
    #[error("the federation peer is unavailable")]
    Unavailable,
    #[error("the federation peer refused the request with HTTP {0}")]
    Refused(u16),
    #[error("invalid signed federation response: {0}")]
    InvalidResponse(String),
}

#[derive(Serialize)]
struct WireRequest<'a> {
    envelope: &'a Envelope,
    payload_base64: String,
}

#[derive(Serialize)]
struct UnsignedResponse<'a> {
    request_id: aseman_domain::Uuid,
    outcome: &'a str,
    answer: &'a Option<String>,
    reason: &'a Option<String>,
}

#[derive(Default)]
struct Circuit {
    consecutive_failures: u32,
    open_until: Option<Instant>,
}

impl Circuit {
    fn admit(&mut self, now: Instant) -> bool {
        match self.open_until {
            Some(until) if now < until => false,
            Some(_) => {
                self.open_until = None;
                true
            }
            None => true,
        }
    }

    fn succeeded(&mut self) {
        self.consecutive_failures = 0;
        self.open_until = None;
    }

    fn failed(&mut self, now: Instant, threshold: u32, open_for: Duration) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= threshold {
            self.open_until = Some(now + open_for);
        }
    }
}

/// The production peer client. It accepts HTTPS endpoints only and installs no
/// platform roots: only the explicitly supplied federation CA is trusted.
pub struct HttpFederationClient {
    endpoint: String,
    http: Client,
    signer: Arc<dyn FederationProofSigner>,
    verifier: Arc<dyn FederationResponseVerifier>,
    config: FederationClientConfig,
    circuit: Mutex<Circuit>,
}

impl HttpFederationClient {
    /// Build a mutually authenticated federation client.
    pub fn new(
        endpoint: &str,
        tls: &FederationTls,
        signer: Arc<dyn FederationProofSigner>,
        verifier: Arc<dyn FederationResponseVerifier>,
        config: FederationClientConfig,
    ) -> Result<Self, FederationClientError> {
        validate(config)?;
        if !endpoint.starts_with("https://") {
            return Err(FederationClientError::InvalidConfiguration(
                "the endpoint must use https",
            ));
        }
        let identity = reqwest::Identity::from_pem(&tls.identity_pem)
            .map_err(|error| FederationClientError::InvalidResponse(error.to_string()))?;
        let mut builder = Client::builder()
            .use_rustls_tls()
            .tls_built_in_root_certs(false)
            .https_only(true)
            .identity(identity)
            .timeout(config.deadline)
            .connect_timeout(config.deadline.min(Duration::from_secs(5)));
        for root in reqwest::Certificate::from_pem_bundle(&tls.server_roots_pem)
            .map_err(|error| FederationClientError::InvalidResponse(error.to_string()))?
        {
            builder = builder.add_root_certificate(root);
        }
        let http = builder
            .build()
            .map_err(|error| FederationClientError::InvalidResponse(error.to_string()))?;
        Ok(Self {
            endpoint: format!("{}/v1/federation/envelopes", endpoint.trim_end_matches('/')),
            http,
            signer,
            verifier,
            config,
            circuit: Mutex::new(Circuit::default()),
        })
    }

    /// Send one idempotent envelope and verify the destination's signed answer.
    pub fn send(
        &self,
        envelope: &Envelope,
        payload: &[u8],
    ) -> Result<SignedFederationResponse, FederationClientError> {
        if !self.lock_circuit().admit(Instant::now()) {
            return Err(FederationClientError::CircuitOpen);
        }
        let proof = self
            .signer
            .proof_header(envelope, payload)
            .map_err(FederationClientError::Signing)?;
        let body = WireRequest {
            envelope,
            payload_base64: base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                payload,
            ),
        };

        for attempt in 0..self.config.attempts {
            let result = self
                .http
                .post(&self.endpoint)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(PROOF_HEADER, &proof)
                .json(&body)
                .send();
            match result {
                Ok(response) if response.status().is_success() => {
                    let response: SignedFederationResponse = response
                        .json()
                        .map_err(|error| self.invalid(error.to_string()))?;
                    self.verify(envelope, &response)?;
                    self.lock_circuit().succeeded();
                    return Ok(response);
                }
                Ok(response) if retryable(response.status()) => {}
                Ok(response) => {
                    self.lock_circuit().succeeded();
                    return Err(FederationClientError::Refused(response.status().as_u16()));
                }
                Err(_) => {}
            }
            if attempt + 1 < self.config.attempts {
                std::thread::sleep(backoff(&self.config, attempt));
            }
        }
        self.lock_circuit().failed(
            Instant::now(),
            self.config.circuit_failure_threshold,
            self.config.circuit_open_for,
        );
        Err(FederationClientError::Unavailable)
    }

    fn verify(
        &self,
        envelope: &Envelope,
        response: &SignedFederationResponse,
    ) -> Result<(), FederationClientError> {
        if response.request_id != envelope.request_id {
            return Err(self.invalid("response request ID does not match"));
        }
        if !matches!(
            response.outcome.as_str(),
            "executed" | "replayed" | "refused"
        ) {
            return Err(self.invalid("unknown response outcome"));
        }
        let bytes = serde_json::to_vec(&UnsignedResponse {
            request_id: response.request_id,
            outcome: &response.outcome,
            answer: &response.answer,
            reason: &response.reason,
        })
        .map_err(|error| self.invalid(error.to_string()))?;
        self.verifier
            .verify(&bytes, &response.signature)
            .map_err(|error| self.invalid(error))
    }

    fn invalid(&self, reason: impl Into<String>) -> FederationClientError {
        self.lock_circuit().failed(
            Instant::now(),
            self.config.circuit_failure_threshold,
            self.config.circuit_open_for,
        );
        FederationClientError::InvalidResponse(reason.into())
    }

    fn lock_circuit(&self) -> std::sync::MutexGuard<'_, Circuit> {
        self.circuit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

struct NodeProofSigner {
    node_id: Uuid,
    key_epoch: u32,
    key_id: String,
    key: Arc<Ed25519KeyPair>,
    audience: String,
}

impl FederationProofSigner for NodeProofSigner {
    fn proof_header(&self, envelope: &Envelope, payload: &[u8]) -> Result<String, String> {
        if envelope.source_node != self.node_id {
            return Err("the envelope source does not match the node credential".to_owned());
        }
        let subject = Subject {
            kind: SubjectKind::Node,
            id: self.node_id,
        };
        let digest = body_digest(payload);
        let proof = sign_ed25519(
            self.key.as_ref(),
            SignatureContext::Request,
            &SignedFields {
                algorithm: "ed25519",
                key_id: &self.key_id,
                key_epoch: self.key_epoch,
                subject: &subject,
                audience: &self.audience,
                window: CredentialWindow {
                    issued_at_millis: envelope.issued_at_millis,
                    not_before_millis: envelope.issued_at_millis,
                    expires_at_millis: envelope.expires_at_millis,
                },
                nonce: envelope.nonce.as_bytes(),
                request_id: &envelope.request_id.to_string(),
                action: &envelope.action,
                resource: &envelope.target,
                body_digest: &digest,
            },
        );
        serde_json::to_vec(&proof)
            .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
            .map_err(|error| error.to_string())
    }
}

struct DescriptorResponseVerifier {
    keys: Vec<Vec<u8>>,
}

impl DescriptorResponseVerifier {
    fn new(descriptor: &NodeDescriptor) -> Result<Self, FederationClientError> {
        if descriptor.revoked_epochs.contains(&descriptor.key_epoch) {
            return Err(FederationClientError::InvalidResponse(
                "the destination descriptor's current epoch is revoked".to_owned(),
            ));
        }
        let keys = descriptor
            .keys
            .iter()
            .map(|encoded| {
                URL_SAFE_NO_PAD
                    .decode(encoded)
                    .map_err(|_| {
                        FederationClientError::InvalidResponse(
                            "a destination descriptor key is not base64url".to_owned(),
                        )
                    })
                    .and_then(|key| {
                        if key.len() == 32 {
                            Ok(key)
                        } else {
                            Err(FederationClientError::InvalidResponse(
                                "a destination descriptor key is not Ed25519".to_owned(),
                            ))
                        }
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if keys.is_empty() {
            return Err(FederationClientError::InvalidResponse(
                "the destination descriptor has no active key".to_owned(),
            ));
        }
        Ok(Self { keys })
    }
}

impl FederationResponseVerifier for DescriptorResponseVerifier {
    fn verify(&self, message: &[u8], signature: &str) -> Result<(), String> {
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| "the response signature is not base64url".to_owned())?;
        if self.keys.iter().any(|key| {
            UnparsedPublicKey::new(&ED25519, key)
                .verify(message, &signature)
                .is_ok()
        }) {
            Ok(())
        } else {
            Err("the response signature does not match the destination descriptor".to_owned())
        }
    }
}

struct CachedClient {
    sequence: u64,
    client: Arc<HttpFederationClient>,
}

/// A descriptor-routed federation transport used by the application sender.
///
/// One circuit breaker is retained per destination node. A descriptor sequence change
/// replaces the cached endpoint and verifier atomically on the next request.
pub struct DescriptorHttpTransport {
    tls_roots: Vec<u8>,
    tls_identity: Vec<u8>,
    node_id: Uuid,
    key_epoch: u32,
    key_id: String,
    key: Arc<Ed25519KeyPair>,
    config: FederationClientConfig,
    clients: Mutex<HashMap<Uuid, CachedClient>>,
}

impl DescriptorHttpTransport {
    pub fn new(
        tls: FederationTls,
        credential: FederationNodeCredential,
        config: FederationClientConfig,
    ) -> Result<Self, FederationClientError> {
        validate(config)?;
        let key = Ed25519KeyPair::from_pkcs8(&credential.signing_key_pkcs8).map_err(|_| {
            FederationClientError::InvalidConfiguration(
                "the request signing key must be Ed25519 PKCS#8",
            )
        })?;
        let public: [u8; 32] = key.public_key().as_ref().try_into().map_err(|_| {
            FederationClientError::InvalidConfiguration("the request signing key is malformed")
        })?;
        let key_id = PublicKey::ed25519(public).key_id();
        Ok(Self {
            tls_roots: tls.server_roots_pem,
            tls_identity: tls.identity_pem,
            node_id: credential.node_id,
            key_epoch: credential.key_epoch,
            key_id,
            key: Arc::new(key),
            config,
            clients: Mutex::new(HashMap::new()),
        })
    }

    /// Raw Ed25519 public key in the A704 descriptor encoding.
    #[must_use]
    pub fn descriptor_public_key(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.key.public_key().as_ref())
    }

    fn client(
        &self,
        destination: &NodeDescriptor,
    ) -> Result<Arc<HttpFederationClient>, FederationClientError> {
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = clients.get(&destination.node_id)
            && cached.sequence == destination.sequence
        {
            return Ok(cached.client.clone());
        }
        let signer = Arc::new(NodeProofSigner {
            node_id: self.node_id,
            key_epoch: self.key_epoch,
            key_id: self.key_id.clone(),
            key: self.key.clone(),
            audience: federation_audience(destination.node_id),
        });
        let verifier = Arc::new(DescriptorResponseVerifier::new(destination)?);
        let client = Arc::new(HttpFederationClient::new(
            &destination.federation_endpoint,
            &FederationTls {
                server_roots_pem: self.tls_roots.clone(),
                identity_pem: self.tls_identity.clone(),
            },
            signer,
            verifier,
            self.config,
        )?);
        clients.insert(
            destination.node_id,
            CachedClient {
                sequence: destination.sequence,
                client: client.clone(),
            },
        );
        Ok(client)
    }
}

impl Transport for DescriptorHttpTransport {
    fn send(
        &self,
        destination: &NodeDescriptor,
        envelope: &Envelope,
        payload: &[u8],
    ) -> Result<FederationReply, PortError> {
        if destination.node_id != envelope.destination_node {
            return Err(PortError::Denied(
                "federation descriptor does not match destination",
            ));
        }
        let response = self
            .client(destination)
            .and_then(|client| client.send(envelope, payload))
            .map_err(|error| match error {
                FederationClientError::CircuitOpen => PortError::Unavailable("peer circuit open"),
                FederationClientError::Unavailable => {
                    PortError::Unavailable("federation peer unavailable")
                }
                FederationClientError::Refused(_) => {
                    PortError::Denied("federation transport refused request")
                }
                FederationClientError::InvalidConfiguration(_) => {
                    PortError::Unavailable("invalid federation client configuration")
                }
                FederationClientError::Signing(reason)
                | FederationClientError::InvalidResponse(reason) => PortError::Failed(reason),
            })?;
        match response.outcome.as_str() {
            "executed" => response
                .answer
                .map(FederationReply::Executed)
                .ok_or_else(|| PortError::Failed("executed response has no answer".to_owned())),
            "replayed" => response
                .answer
                .map(FederationReply::Replayed)
                .ok_or_else(|| PortError::Failed("replayed response has no answer".to_owned())),
            "refused" => Ok(FederationReply::Refused(
                response
                    .reason
                    .unwrap_or_else(|| "destination_refused".to_owned()),
            )),
            _ => Err(PortError::Failed(
                "federation response has an unknown outcome".to_owned(),
            )),
        }
    }
}

/// The deterministic A401 audience advertised by every Aseman federation node.
#[must_use]
pub fn federation_audience(node_id: Uuid) -> String {
    format!("node:{node_id}/federation/v1")
}

fn validate(config: FederationClientConfig) -> Result<(), FederationClientError> {
    if config.attempts == 0 {
        return Err(FederationClientError::InvalidConfiguration(
            "attempts must be nonzero",
        ));
    }
    if config.circuit_failure_threshold == 0 {
        return Err(FederationClientError::InvalidConfiguration(
            "circuit failure threshold must be nonzero",
        ));
    }
    if config.deadline.is_zero() {
        return Err(FederationClientError::InvalidConfiguration(
            "deadline must be nonzero",
        ));
    }
    Ok(())
}

fn retryable(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn backoff(config: &FederationClientConfig, attempt: u32) -> Duration {
    let multiplier = 1_u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
    config
        .initial_backoff
        .saturating_mul(multiplier)
        .min(config.maximum_backoff)
}

#[cfg(test)]
mod tests {
    use aseman_contracts::guest_api::parse_proof_header;
    use aseman_contracts::identity::verify_proof_signature;

    use super::*;

    fn envelope(source: Uuid, destination: Uuid) -> Envelope {
        Envelope {
            request_id: Uuid::now_v7(),
            source_node: source,
            destination_node: destination,
            subject: format!("workload:{}", Uuid::now_v7()),
            target: format!("workload:{}", Uuid::now_v7()),
            action: "workload.signal".to_owned(),
            payload_digest: format!("sha256:{}", "0".repeat(64)),
            issued_at_millis: 1_000,
            expires_at_millis: 61_000,
            nonce: Uuid::now_v7().to_string(),
            hop_limit: 0,
            version: "1".to_owned(),
        }
    }

    #[test]
    fn backoff_is_exponential_and_bounded() {
        let config = FederationClientConfig {
            initial_backoff: Duration::from_millis(10),
            maximum_backoff: Duration::from_millis(25),
            ..FederationClientConfig::default()
        };
        assert_eq!(backoff(&config, 0), Duration::from_millis(10));
        assert_eq!(backoff(&config, 1), Duration::from_millis(20));
        assert_eq!(backoff(&config, 2), Duration::from_millis(25));
    }

    #[test]
    fn circuit_opens_at_threshold_and_recovers_after_deadline() {
        let now = Instant::now();
        let mut circuit = Circuit::default();
        circuit.failed(now, 2, Duration::from_secs(5));
        assert!(circuit.admit(now));
        circuit.failed(now, 2, Duration::from_secs(5));
        assert!(!circuit.admit(now + Duration::from_secs(4)));
        assert!(circuit.admit(now + Duration::from_secs(5)));
        circuit.succeeded();
        assert!(circuit.admit(now));
    }

    #[test]
    fn invalid_configuration_fails_closed() {
        let config = FederationClientConfig {
            attempts: 0,
            ..FederationClientConfig::default()
        };
        assert_eq!(
            validate(config),
            Err(FederationClientError::InvalidConfiguration(
                "attempts must be nonzero"
            ))
        );
    }

    #[test]
    fn node_proof_binds_the_exact_envelope_payload_and_peer_audience() {
        let key = Arc::new(Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap());
        let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let source = Uuid::now_v7();
        let destination = Uuid::now_v7();
        let signer = NodeProofSigner {
            node_id: source,
            key_epoch: 3,
            key_id: PublicKey::ed25519(public).key_id(),
            key,
            audience: federation_audience(destination),
        };
        let envelope = envelope(source, destination);
        let proof = parse_proof_header(&signer.proof_header(&envelope, b"payload").unwrap())
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(proof.subject.to_string(), format!("node:{source}"));
        assert_eq!(proof.request_id, envelope.request_id.to_string());
        assert_eq!(proof.action, envelope.action);
        assert_eq!(proof.resource, envelope.target);
        assert_eq!(proof.audience, federation_audience(destination));
        assert_eq!(proof.body_digest, body_digest(b"payload"));
        verify_proof_signature(&proof, &PublicKey::ed25519(public)).unwrap();
    }

    #[test]
    fn response_verifier_uses_only_active_descriptor_keys() {
        let key = Ed25519KeyPair::from_seed_unchecked(&[8; 32]).unwrap();
        let descriptor = NodeDescriptor {
            node_id: Uuid::now_v7(),
            key_epoch: 2,
            keys: vec![URL_SAFE_NO_PAD.encode(key.public_key().as_ref())],
            federation_endpoint: "https://peer.invalid".to_owned(),
            client_endpoint: "https://peer.invalid".to_owned(),
            contracts: vec!["a705/1".to_owned()],
            runtimes: Vec::new(),
            sequence: 4,
            expires_at_millis: 100_000,
            revoked_epochs: Vec::new(),
        };
        let verifier = DescriptorResponseVerifier::new(&descriptor).unwrap();
        let message = b"signed answer";
        assert!(
            verifier
                .verify(message, &URL_SAFE_NO_PAD.encode(key.sign(message).as_ref()))
                .is_ok()
        );
        assert!(
            verifier
                .verify(message, &URL_SAFE_NO_PAD.encode([0; 64]))
                .is_err()
        );
        let mut revoked = descriptor;
        revoked.revoked_epochs.push(revoked.key_epoch);
        assert!(DescriptorResponseVerifier::new(&revoked).is_err());
    }
}
