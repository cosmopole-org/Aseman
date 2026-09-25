//! The composed [`PublicActionService`] (P7-06): the A701 transport admits routes and
//! parses authentication; this crate binds that edge to the application use case that
//! authenticates (A401), authorizes (A402), executes, and settles durable idempotency.
//!
//! The transport never calls a legacy handler directly. The [`ActionExecutor`] the
//! node shell supplies resolves each action's resource and runs it against migrated
//! use cases one family at a time (RL-004).
#![forbid(unsafe_code)]

use std::sync::Arc;

use aseman_application::identity::VerifierPolicy;
use aseman_application::public_action::{
    PublicActionFailure, PublicActionRequest, RequestAuthentication, ServePublicAction,
};
use aseman_ports::{
    ActionExecutor, ClockPort, DecisionAudit, GrantStore, IdentityVerifier, KeyDirectory,
    PolicyDecisionPort, PortError, PublicActionIdempotency, ReplayGuard, SessionDirectory,
};

pub use aseman_public_http::{
    Authentication, PublicActionError, PublicActionResponse, PublicActionService,
};

/// The one composed service behind the public protocol.
pub struct ComposedPublicActionService {
    keys: Arc<dyn KeyDirectory>,
    replay: Arc<dyn ReplayGuard>,
    sessions: Arc<dyn SessionDirectory>,
    verifier: Arc<dyn IdentityVerifier>,
    clock: Arc<dyn ClockPort>,
    policy: Arc<dyn PolicyDecisionPort>,
    grants: Arc<dyn GrantStore>,
    audit: Arc<dyn DecisionAudit>,
    idempotency: Arc<dyn PublicActionIdempotency>,
    executor: Arc<dyn ActionExecutor>,
    verifier_policy: VerifierPolicy,
}

impl ComposedPublicActionService {
    /// Compose the service from its ports.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        keys: Arc<dyn KeyDirectory>,
        replay: Arc<dyn ReplayGuard>,
        sessions: Arc<dyn SessionDirectory>,
        verifier: Arc<dyn IdentityVerifier>,
        clock: Arc<dyn ClockPort>,
        policy: Arc<dyn PolicyDecisionPort>,
        grants: Arc<dyn GrantStore>,
        audit: Arc<dyn DecisionAudit>,
        idempotency: Arc<dyn PublicActionIdempotency>,
        executor: Arc<dyn ActionExecutor>,
        verifier_policy: VerifierPolicy,
    ) -> Self {
        Self {
            keys,
            replay,
            sessions,
            verifier,
            clock,
            policy,
            grants,
            audit,
            idempotency,
            executor,
            verifier_policy,
        }
    }

    fn serve(
        &self,
        request: aseman_public_http::PublicActionRequest,
    ) -> Result<Vec<u8>, PublicActionFailure> {
        let aseman_public_http::PublicActionRequest {
            request_id,
            route,
            action,
            class,
            authentication,
            idempotency_key,
            body,
        } = request;
        let authentication = match authentication {
            Authentication::Session(session) => RequestAuthentication::Session(session),
            Authentication::Proof(proof) => RequestAuthentication::Proof(proof),
        };
        let input = PublicActionRequest {
            request_id,
            route,
            action,
            class,
            authentication,
            idempotency_key,
            body,
        };
        ServePublicAction {
            keys: self.keys.as_ref(),
            replay: self.replay.as_ref(),
            sessions: self.sessions.as_ref(),
            verifier: self.verifier.as_ref(),
            clock: self.clock.as_ref(),
            policy: self.policy.as_ref(),
            grants: self.grants.as_ref(),
            audit: self.audit.as_ref(),
            idempotency: self.idempotency.as_ref(),
            executor: self.executor.as_ref(),
            verifier_policy: &self.verifier_policy,
        }
        .execute(&input)
        .map(|response| response.body)
    }
}

impl PublicActionService for ComposedPublicActionService {
    fn invoke(
        &self,
        request: aseman_public_http::PublicActionRequest,
    ) -> Result<PublicActionResponse, PublicActionError> {
        match self.serve(request) {
            Ok(body) => Ok(PublicActionResponse { status: 200, body }),
            Err(failure) => Err(into_error(failure)),
        }
    }
}

/// The stable RFC 9457 error for an application failure.
fn into_error(failure: PublicActionFailure) -> PublicActionError {
    match failure {
        PublicActionFailure::Rejected(code) => error(401, code.code(), ""),
        PublicActionFailure::Refused(reason) => error(401, "refused", reason),
        PublicActionFailure::Denied(reason) => error(403, "denied", &reason),
        PublicActionFailure::IdempotencyInProgress => error(409, "idempotency_in_progress", ""),
        PublicActionFailure::IdempotencyMismatch => error(409, "idempotency_mismatch", ""),
        PublicActionFailure::Unavailable(port_error) => match port_error {
            PortError::NotFound => error(404, "not_found", ""),
            PortError::Denied(reason) => error(403, "denied", reason),
            PortError::Unsupported(reason) => error(422, "unsupported", reason),
            PortError::Deadline => error(504, "deadline_exceeded", ""),
            other => error(503, "unavailable", &other.to_string()),
        },
    }
}

fn error(status: u16, reason: &str, detail: &str) -> PublicActionError {
    PublicActionError {
        status,
        reason: reason.to_owned(),
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests;
