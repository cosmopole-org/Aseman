//! Serving a federated request (A705).
//!
//! The rule this use case exists to enforce: **the destination decides.** A source
//! node authorizes a request locally before it sends one, but that authorization
//! means nothing here. This node checks the envelope against its own clock and its own
//! record of what it has seen, then reauthorizes the subject and action against its own
//! policy, and only then executes.
//!
//! A forbidden operation therefore fails at the destination, whatever the source
//! believed — which is exactly what two independently
//! administered clusters to demonstrate.

use aseman_domain::Uuid;
use std::collections::BTreeSet;

use aseman_domain::authority::{Condition, DecisionReason, PolicyRequest, ResourceRef};
use aseman_domain::federation::{
    Envelope, FederationError, FederationReply, MAX_LIFETIME_MILLIS, accept,
};
use aseman_domain::identity::{Subject, SubjectKind};
use aseman_ports::federation::{Directory, EnvelopeGuard, Transport};
use aseman_ports::{ClockPort, PolicyDecisionPort, PortError};
use sha2::{Digest, Sha256};

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

/// Resolve, locally authorize, and send one canonical cross-node action.
pub struct SendFederatedRequest<'a> {
    pub directory: &'a dyn Directory,
    pub transport: &'a dyn Transport,
    pub policy: &'a dyn PolicyDecisionPort,
    pub clock: &'a dyn ClockPort,
    pub node_id: Uuid,
}

/// One canonical cross-node action, as the source node sends it.
pub struct FederatedAction<'a> {
    pub request_id: Uuid,
    pub subject: Subject,
    pub destination_node: Uuid,
    pub target: ResourceRef,
    pub action: &'a str,
    pub payload: &'a [u8],
    pub facts: BTreeSet<Condition>,
}

impl SendFederatedRequest<'_> {
    /// Send an action to its destination node after the source node authorizes the
    /// same subject/action/target tuple the destination will independently reauthorize.
    pub fn send(&self, request: FederatedAction<'_>) -> Result<FederationReply, PortError> {
        let FederatedAction {
            request_id,
            subject,
            destination_node,
            target,
            action,
            payload,
            facts,
        } = request;
        if !matches!(subject.kind, SubjectKind::Workload | SubjectKind::Creature) {
            return Err(PortError::Denied("invalid federated subject"));
        }
        if destination_node == self.node_id {
            return Err(PortError::Denied("federation destination is this node"));
        }
        let now = self.clock.unix_millis();
        let destination = self
            .directory
            .node(destination_node, now)?
            .ok_or(PortError::NotFound)?;
        let decision = self.policy.decide(&PolicyRequest {
            subject: Some(subject),
            action: action.to_owned(),
            resource: target.clone(),
            facts,
            grants: Vec::new(),
            at_millis: now,
        })?;
        if !decision.allowed {
            return Err(PortError::Denied("source policy denied federation"));
        }
        let envelope = Envelope {
            request_id,
            source_node: self.node_id,
            destination_node,
            subject: subject.to_string(),
            target: format!("{}:{}", target.kind, target.id),
            action: action.to_owned(),
            payload_digest: format!("sha256:{}", hex::encode(Sha256::digest(payload))),
            issued_at_millis: now,
            expires_at_millis: now.saturating_add(MAX_LIFETIME_MILLIS),
            nonce: request_id.to_string(),
            hop_limit: 0,
            version: "1".to_owned(),
        };
        self.transport.send(&destination, &envelope, payload)
    }
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
        let answer = match execute(envelope) {
            Ok(answer) => answer,
            Err(error) => {
                self.guard.forget_nonce(envelope)?;
                return Err(error);
            }
        };
        self.guard
            .record_answer(envelope.request_id, &answer, envelope.expires_at_millis)?;
        Ok(Served::Executed(answer))
    }
}

#[cfg(test)]
mod outbound_tests {
    use std::sync::Mutex;

    use aseman_domain::authority::{PolicyDecision, PolicyRequest};
    use aseman_domain::federation::{NodeDescriptor, WorkloadDescriptor};
    use aseman_ports::PortResult;

    use super::*;

    struct FixedClock(i64);

    impl ClockPort for FixedClock {
        fn unix_millis(&self) -> i64 {
            self.0
        }
    }

    struct TestDirectory {
        own: NodeDescriptor,
        peer: NodeDescriptor,
    }

    impl Directory for TestDirectory {
        fn own_node(&self) -> PortResult<NodeDescriptor> {
            Ok(self.own.clone())
        }

        fn node(&self, node_id: Uuid, now_millis: i64) -> PortResult<Option<NodeDescriptor>> {
            Ok(
                (node_id == self.peer.node_id && now_millis < self.peer.expires_at_millis)
                    .then(|| self.peer.clone()),
            )
        }

        fn record_node(&self, _: &NodeDescriptor) -> PortResult<()> {
            unreachable!()
        }

        fn workload(&self, _: Uuid, _: i64) -> PortResult<Option<WorkloadDescriptor>> {
            Ok(None)
        }

        fn record_workload(&self, _: &WorkloadDescriptor) -> PortResult<()> {
            unreachable!()
        }
    }

    struct Allow;

    impl PolicyDecisionPort for Allow {
        fn decide(&self, request: &PolicyRequest) -> PortResult<PolicyDecision> {
            Ok(PolicyDecision {
                allowed: request.facts.contains(&Condition::Authenticated),
                reason: DecisionReason::Allowed,
                matched: Some(Condition::Authenticated),
                considered: vec![Condition::Authenticated],
                grant_chain: Vec::new(),
                registry_version: "test".to_owned(),
                policy_version: "test".to_owned(),
            })
        }
    }

    struct RecordingTransport(Mutex<Option<(NodeDescriptor, Envelope, Vec<u8>)>>);

    impl Transport for RecordingTransport {
        fn send(
            &self,
            destination: &NodeDescriptor,
            envelope: &Envelope,
            payload: &[u8],
        ) -> PortResult<FederationReply> {
            *self.0.lock().unwrap() =
                Some((destination.clone(), envelope.clone(), payload.to_vec()));
            Ok(FederationReply::Executed("answer".to_owned()))
        }
    }

    fn descriptor(node_id: Uuid) -> NodeDescriptor {
        NodeDescriptor {
            node_id,
            key_epoch: 1,
            keys: vec!["key".to_owned()],
            federation_endpoint: "https://peer.invalid".to_owned(),
            client_endpoint: "https://peer.invalid".to_owned(),
            contracts: vec!["a705/1".to_owned()],
            runtimes: Vec::new(),
            sequence: 1,
            expires_at_millis: 100_000,
            revoked_epochs: Vec::new(),
        }
    }

    #[test]
    fn outbound_sender_authorizes_resolves_and_builds_the_canonical_envelope() {
        let source = Uuid::now_v7();
        let destination = Uuid::now_v7();
        let directory = TestDirectory {
            own: descriptor(source),
            peer: descriptor(destination),
        };
        let transport = RecordingTransport(Mutex::new(None));
        let subject = Subject {
            kind: SubjectKind::Workload,
            id: Uuid::now_v7(),
        };
        let target = ResourceRef {
            kind: "workload".to_owned(),
            id: Uuid::now_v7().to_string(),
        };
        let request_id = Uuid::now_v7();
        let reply = SendFederatedRequest {
            directory: &directory,
            transport: &transport,
            policy: &Allow,
            clock: &FixedClock(4_000),
            node_id: source,
        }
        .send(FederatedAction {
            request_id,
            subject,
            destination_node: destination,
            target: target.clone(),
            action: "workload.signal",
            payload: b"payload",
            facts: BTreeSet::from([Condition::Authenticated]),
        })
        .unwrap();
        assert_eq!(reply, FederationReply::Executed("answer".to_owned()));
        let (_, envelope, payload) = transport.0.lock().unwrap().clone().unwrap();
        assert_eq!(envelope.request_id, request_id);
        assert_eq!(envelope.source_node, source);
        assert_eq!(envelope.destination_node, destination);
        assert_eq!(envelope.subject, subject.to_string());
        assert_eq!(envelope.target, format!("workload:{}", target.id));
        assert_eq!(envelope.issued_at_millis, 4_000);
        assert_eq!(envelope.expires_at_millis, 64_000);
        assert_eq!(payload, b"payload");
        assert_eq!(
            envelope.payload_digest,
            format!("sha256:{}", hex::encode(Sha256::digest(b"payload")))
        );
    }
}
