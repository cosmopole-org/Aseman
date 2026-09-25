//! The composed public action service (P7-06): A401 authentication (proof or session),
//! A402 authorization, execution, and durable idempotency as one application path.
//!
//! The transport admits routes against the generated A701 contract and parses exactly
//! one session or proof; this use case is what it calls. It never calls a legacy
//! handler directly — the [`ActionExecutor`] seam owns the resource resolution and the
//! actual effect, so the node shell can migrate handlers one family at a time
//! (RL-004).

use aseman_domain::authority::{ActionClass, AuditRecord};
use aseman_domain::identity::{AuthenticationError, Proof, Subject};
use aseman_ports::{
    ActionExecutor, ClockPort, DecisionAudit, GrantStore, IdentityVerifier, KeyDirectory,
    PolicyDecisionPort, PortError, PublicActionClaim, PublicActionIdempotency, ReplayGuard,
    SessionDirectory,
};
use thiserror::Error;

use crate::capability::Authorize;
use crate::identity::{AuthenticateProof, IdentityFailure, VerifierPolicy};

/// Authentication material at the use-case boundary.
#[derive(Clone, Debug)]
pub enum RequestAuthentication {
    Session(String),
    Proof(Box<Proof>),
}

/// One public action request, fully formed for the use case.
#[derive(Clone, Debug)]
pub struct PublicActionRequest {
    pub request_id: String,
    pub route: String,
    pub action: String,
    pub class: ActionClass,
    pub authentication: RequestAuthentication,
    pub idempotency_key: Option<String>,
    pub body: Vec<u8>,
}

/// The public action result after authentication, authorization, and execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicActionResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Why a public action did not produce a result.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PublicActionFailure {
    /// A proof is refused with an A401 code.
    #[error("authentication failed: {}", .0.code())]
    Rejected(AuthenticationError),
    /// The credential, session, or request does not qualify.
    #[error("refused: {0}")]
    Refused(&'static str),
    /// A402: the decision denied the action.
    #[error("not authorized: {0}")]
    Denied(String),
    /// A mutation was retried while its first attempt is still in flight.
    #[error("the mutation is already in progress")]
    IdempotencyInProgress,
    /// The idempotency key was already used for a different request.
    #[error("the idempotency key was used for a different request")]
    IdempotencyMismatch,
    /// A directory, store, or executor could not answer. Nothing was accepted.
    #[error(transparent)]
    Unavailable(#[from] PortError),
}

/// Run one admitted public action.
pub struct ServePublicAction<'a> {
    pub keys: &'a dyn KeyDirectory,
    pub replay: &'a dyn ReplayGuard,
    pub sessions: &'a dyn SessionDirectory,
    pub verifier: &'a dyn IdentityVerifier,
    pub clock: &'a dyn ClockPort,
    pub policy: &'a dyn PolicyDecisionPort,
    pub grants: &'a dyn GrantStore,
    pub audit: &'a dyn DecisionAudit,
    pub idempotency: &'a dyn PublicActionIdempotency,
    pub executor: &'a dyn ActionExecutor,
    pub verifier_policy: &'a VerifierPolicy,
}

impl ServePublicAction<'_> {
    /// Authenticate, authorize, execute, and (for mutations) settle idempotency.
    ///
    /// # Errors
    ///
    /// An A401 code, a refusal, an A402 denial, or a provider failure.
    pub fn execute(
        &self,
        request: &PublicActionRequest,
    ) -> Result<PublicActionResponse, PublicActionFailure> {
        let subject = self.authenticate(request)?;
        // A402: the executor resolves the resource and the caller-established facts
        // about it; policy decides.
        let (resource, facts) = self
            .executor
            .resolve(&subject, &request.action, &request.body)?;
        if let RequestAuthentication::Proof(proof) = &request.authentication
            && proof.resource != resource.id
        {
            return Err(PublicActionFailure::Refused(
                "the credential does not cover this resource",
            ));
        }
        let decision = Authorize {
            policy: self.policy,
            grants: self.grants,
            clock: self.clock,
        }
        .decide(Some(subject), &request.action, resource.clone(), facts)?;
        self.audit.record(&AuditRecord {
            actor: subject.to_string(),
            action: request.action.clone(),
            target: format!("{}:{}", resource.kind, resource.id),
            decision: if decision.allowed {
                "allowed"
            } else {
                decision.reason.code()
            }
            .to_owned(),
            occurred_at_millis: self.clock.unix_millis(),
            details: format!(
                "route={} request_id={} matched={} registry_version={} policy_version={}",
                request.route,
                request.request_id,
                decision.matched.map_or_else(
                    || "none".to_owned(),
                    |condition| condition.as_str().to_owned()
                ),
                decision.registry_version,
                decision.policy_version,
            ),
        })?;
        if !decision.allowed {
            return Err(PublicActionFailure::Denied(
                decision.reason.code().to_owned(),
            ));
        }
        // A mutation's effect runs once under its idempotency key. Recheck this at the
        // use-case boundary so a non-HTTP caller cannot bypass A701's requirement.
        if request.class != ActionClass::Read {
            let key = request
                .idempotency_key
                .as_deref()
                .ok_or(PublicActionFailure::Refused("idempotency key required"))?;
            return self.execute_mutation(request, subject, key);
        }
        let body = self
            .executor
            .execute(subject, &request.action, &request.body)?;
        Ok(PublicActionResponse { status: 200, body })
    }

    fn authenticate(&self, request: &PublicActionRequest) -> Result<Subject, PublicActionFailure> {
        match &request.authentication {
            RequestAuthentication::Session(token) => self
                .sessions
                .subject(token)?
                .ok_or(PublicActionFailure::Refused("unknown session")),
            RequestAuthentication::Proof(proof) => {
                let subject = AuthenticateProof {
                    keys: self.keys,
                    replay: self.replay,
                    verifier: self.verifier,
                    clock: self.clock,
                }
                .execute(proof, &request.body, self.verifier_policy)?;
                // The proof must bind the action it is used for.
                if proof.action != request.action {
                    return Err(PublicActionFailure::Refused(
                        "the credential does not cover this action",
                    ));
                }
                Ok(subject)
            }
        }
    }

    fn execute_mutation(
        &self,
        request: &PublicActionRequest,
        subject: Subject,
        key: &str,
    ) -> Result<PublicActionResponse, PublicActionFailure> {
        let owner = subject.to_string();
        let digest = self.digest(&request.action, &request.body);
        match self.idempotency.claim(&owner, key, digest)? {
            // A retry: replay the first completed outcome instead of repeating the effect.
            PublicActionClaim::Completed(body) => {
                return Ok(PublicActionResponse { status: 200, body });
            }
            PublicActionClaim::InProgress => {
                return Err(PublicActionFailure::IdempotencyInProgress);
            }
            PublicActionClaim::Mismatch => {
                return Err(PublicActionFailure::IdempotencyMismatch);
            }
            PublicActionClaim::Claimed => {}
        }
        match self
            .executor
            .execute(subject, &request.action, &request.body)
        {
            Ok(body) => {
                self.idempotency
                    .complete(&owner, key, &body)
                    .map_err(|_| PortError::Unavailable("idempotency store"))?;
                Ok(PublicActionResponse { status: 200, body })
            }
            Err(error) => {
                // The claim is released so the mutation can be retried; a completed
                // outcome is never fabricated for a failure.
                let _ = self.idempotency.release(&owner, key);
                Err(PublicActionFailure::Unavailable(error))
            }
        }
    }

    /// A request digest that distinguishes one mutation's request from another under
    /// the same key: SHA-256 of `action \0 body`.
    fn digest(&self, action: &str, body: &[u8]) -> [u8; 32] {
        let mut buffer = Vec::with_capacity(action.len() + 1 + body.len());
        buffer.extend_from_slice(action.as_bytes());
        buffer.push(0);
        buffer.extend_from_slice(body);
        self.verifier.body_digest(&buffer)
    }
}

impl From<IdentityFailure> for PublicActionFailure {
    fn from(failure: IdentityFailure) -> Self {
        match failure {
            IdentityFailure::Rejected(error) => Self::Rejected(error),
            IdentityFailure::Refused(message) => Self::Refused(message),
            IdentityFailure::Unavailable(error) => Self::Unavailable(error),
        }
    }
}

#[cfg(test)]
mod tests;
