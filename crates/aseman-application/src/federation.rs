//! Serving a federated request (A705).
//!
//! The rule this use case exists to enforce: **the destination decides.** A source
//! node authorizes a request locally before it sends one, but that authorization
//! means nothing here. This node checks the envelope against its own clock and its own
//! record of what it has seen, then reauthorizes the subject and action against its own
//! policy, and only then executes.
//!
//! A forbidden operation therefore fails at the destination, whatever the source
//! believed — which is exactly what the Phase 7 gate asks two independently
//! administered clusters to demonstrate.

use aseman_domain::Uuid;
use std::collections::BTreeSet;

use aseman_domain::authority::{DecisionReason, PolicyRequest, ResourceRef};
use aseman_domain::federation::{Envelope, FederationError, accept};
use aseman_domain::identity::{Subject, SubjectKind};
use aseman_ports::federation::{Directory, EnvelopeGuard};
use aseman_ports::{ClockPort, PolicyDecisionPort, PortError};

/// What this node did with a federated request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Served {
    /// The request was authorized and executed; this is its answer.
    Executed(String),
    /// A retry of a request this node has already answered. The recorded answer is
    /// returned; nothing was executed a second time.
    Replayed(String),
    /// The destination refused it, with the stable reason a peer can act on.
    Refused(Refusal),
}

/// Why a destination refused a federated request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The envelope itself is not acceptable.
    Envelope(FederationError),
    /// The sending node is not one this node federates with, or its descriptor has
    /// expired.
    UnknownPeer,
    /// The subject may not do this here. `reason` is the policy's own code.
    Denied(DecisionReason),
    /// The subject is not one a federated envelope may carry.
    UnknownSubject,
    /// The target does not name a typed resource.
    UnknownTarget,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Envelope(error) => write!(formatter, "{error}"),
            Self::UnknownPeer => formatter.write_str("the sending node is not known here"),
            Self::Denied(reason) => write!(formatter, "the destination denies this: {reason:?}"),
            Self::UnknownSubject => {
                formatter.write_str("an envelope's subject must be a workload or a creature")
            }
            Self::UnknownTarget => formatter.write_str("an envelope's target must be kind:id"),
        }
    }
}

/// Serve one inbound federated request.
pub struct ServeFederatedRequest<'a> {
    pub directory: &'a dyn Directory,
    pub guard: &'a dyn EnvelopeGuard,
    pub policy: &'a dyn PolicyDecisionPort,
    pub clock: &'a dyn ClockPort,
    /// The node this process is. An envelope addressed elsewhere is refused.
    pub node_id: Uuid,
}

/// The subject an envelope names, when it names one a federation may carry.
///
/// A federated envelope carries a workload or a creature. It never carries a node or a
/// service: those act on their own node, not through one.
fn subject_of(text: &str) -> Option<Subject> {
    let (kind, id) = text.split_once(':')?;
    let kind = match kind {
        "workload" => SubjectKind::Workload,
        "creature" => SubjectKind::Creature,
        _ => return None,
    };
    Some(Subject {
        kind,
        id: id.parse().ok()?,
    })
}

impl ServeFederatedRequest<'_> {
    /// Check, deduplicate, reauthorize, and then execute.
    ///
    /// `execute` runs only when everything above it passed. Its answer is recorded, so
    /// a retry carrying the same request id is answered from the record.
    ///
    /// # Errors
    ///
    /// When a store is unreachable. A refusal is not an error: it is an answer this
    /// node gives the peer.
    pub fn serve(
        &self,
        envelope: &Envelope,
        execute: impl FnOnce(&Envelope) -> Result<String, PortError>,
    ) -> Result<Served, PortError> {
        let now = self.clock.unix_millis();

        // A retry is answered from the record, before anything else: it has already
        // been authorized and executed once, and executing it again is the thing
        // deduplication exists to prevent.
        if let Some(answer) = self.guard.recorded_answer(envelope.request_id)? {
            return Ok(Served::Replayed(answer));
        }

        // The envelope's own shape, against this node's clock. The nonce is recorded
        // as part of this check, so a replay of the same envelope is refused even
        // when it carries a fresh request id.
        let seen = !self.guard.remember_nonce(envelope)?;
        if let Err(error) = accept(envelope, self.node_id, now, seen) {
            return Ok(Served::Refused(Refusal::Envelope(error)));
        }

        // The sending node must be one this node knows and whose descriptor is still
        // fresh. An unknown peer is refused before its subject is even considered.
        if self.directory.node(envelope.source_node, now)?.is_none() {
            return Ok(Served::Refused(Refusal::UnknownPeer));
        }

        let Some(subject) = subject_of(&envelope.subject) else {
            return Ok(Served::Refused(Refusal::UnknownSubject));
        };

        // Reauthorized here, by this node's policy, with **no established facts**.
        //
        // A relational fact — owner, same creature, counterparty — is something the
        // destination works out from its own records about its own resources. An
        // envelope cannot establish one by arriving, however confidently the source
        // asserts it, so none is passed. A federated request therefore reaches only
        // what a rule allows on `public`, `authenticated`, or an explicit grant, and
        // anything relational fails closed until the destination resolves it itself.
        // `kind:id`, so the destination decides against a typed resource rather than
        // an opaque string a peer chose.
        let Some((kind, id)) = envelope.target.split_once(':') else {
            return Ok(Served::Refused(Refusal::UnknownTarget));
        };
        let resource = ResourceRef {
            kind: kind.to_owned(),
            id: id.to_owned(),
        };
        let decision = self.policy.decide(&PolicyRequest {
            subject: Some(subject),
            action: envelope.action.clone(),
            resource,
            facts: BTreeSet::new(),
            grants: Vec::new(),
            at_millis: now,
        })?;
        if !decision.allowed {
            return Ok(Served::Refused(Refusal::Denied(decision.reason)));
        }

        // A failed effect is not an answer and must not poison the durable replay
        // record. The same request ID may be retried after its dependency recovers.
        let answer = execute(envelope)?;
        self.guard
            .record_answer(envelope.request_id, &answer, envelope.expires_at_millis)?;
        Ok(Served::Executed(answer))
    }
}
