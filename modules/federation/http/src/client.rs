//! Outbound federation HTTP with mutual TLS, bounded retries, and a circuit breaker.
//!
//! A retry reuses the same A705 request ID and nonce. The destination's durable
//! envelope guard therefore replays the first answer instead of executing twice.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aseman_domain::federation::Envelope;
use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::Serialize;
use thiserror::Error;

use crate::http::{PROOF_HEADER, SignedFederationResponse};

/// PEM material for authenticating both peers at the transport boundary.
pub struct FederationTls {
    pub server_roots_pem: Vec<u8>,
    /// Client certificate chain followed by its private key.
    pub identity_pem: Vec<u8>,
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
    use super::*;

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
}
