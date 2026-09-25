//! The composed service behind the transport contract (P7-06). Authentication,
//! authorization, idempotency, and audit are proven at the application boundary; here
//! the composed service's own `PublicActionService` mapping and its RFC 9457 error
//! translation are exercised with in-memory ports. Route parsing and the proof header
//! are the transport's tests.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use aseman_domain::authority::{
    ActionClass, Condition, DecisionReason, PolicyDecision, PolicyRequest, ResourceRef,
};
use aseman_domain::capability::Grant;
use aseman_domain::identity::{
    CredentialWindow, FreshnessPolicy, IdentityKey, Introduction, KeyDescription, KeyEpoch,
    KeyPurpose, Proof, RotationPolicy, SignatureContext, Subject, SubjectKind,
};
use aseman_ports::{
    ActionExecutor, ClockPort, DecisionAudit, GrantStore, IdentityVerifier, KeyDirectory,
    PolicyDecisionPort, PortError, PortResult, PublicActionClaim, PublicActionIdempotency,
    ReplayGuard, SessionDirectory,
};

use super::*;

const NOW: i64 = 1_800_000_000_000;

fn subject() -> Subject {
    Subject {
        kind: SubjectKind::Creature,
        id: "0190f1a2-7b3c-7d4e-8f00-112233445566".parse().unwrap(),
    }
}

type IdempotencyState = std::collections::BTreeMap<(String, String), (Vec<u8>, Vec<u8>, bool)>;

#[derive(Default)]
struct World {
    keys: Mutex<std::collections::BTreeMap<String, IdentityKey>>,
    nonces: Mutex<BTreeSet<Vec<u8>>>,
    sessions: Mutex<std::collections::BTreeMap<String, Subject>>,
    allow: bool,
    executed: Mutex<usize>,
    audit: Mutex<Vec<aseman_domain::authority::AuditRecord>>,
    idempotency: Mutex<IdempotencyState>,
}

impl KeyDirectory for World {
    fn key(&self, key_id: &str) -> PortResult<Option<IdentityKey>> {
        Ok(self.keys.lock().unwrap().get(key_id).cloned())
    }
    fn epochs(&self, subject: &Subject, purpose: KeyPurpose) -> PortResult<Vec<IdentityKey>> {
        let mut keys = self
            .keys
            .lock()
            .unwrap()
            .values()
            .filter(|key| key.epoch.subject == *subject && key.epoch.purpose == purpose)
            .cloned()
            .collect::<Vec<_>>();
        keys.sort_by_key(|key| key.epoch.epoch);
        Ok(keys)
    }
    fn register(&self, key: &IdentityKey) -> PortResult<()> {
        let mut keys = self.keys.lock().unwrap();
        if keys.contains_key(&key.key_id)
            || keys.values().any(|stored| {
                stored.epoch.subject == key.epoch.subject
                    && stored.epoch.purpose == key.epoch.purpose
                    && stored.epoch.epoch == key.epoch.epoch
            })
        {
            return Err(PortError::Conflict);
        }
        keys.insert(key.key_id.clone(), key.clone());
        Ok(())
    }
    fn retire(&self, _: &str, _: i64) -> PortResult<()> {
        Ok(())
    }
    fn revoke(&self, _: &str, _: i64) -> PortResult<()> {
        Ok(())
    }
}

impl ReplayGuard for World {
    fn record_nonce(&self, _: &str, nonce: &[u8], _: i64, _: i64) -> PortResult<bool> {
        Ok(self.nonces.lock().unwrap().insert(nonce.to_vec()))
    }
}

impl IdentityVerifier for World {
    fn verify(
        &self,
        proof: &Proof,
        _: &IdentityKey,
    ) -> Result<(), aseman_domain::identity::AuthenticationError> {
        (proof.signature == b"good")
            .then_some(())
            .ok_or(aseman_domain::identity::AuthenticationError::BadSignature)
    }
    fn body_digest(&self, body: &[u8]) -> [u8; 32] {
        let mut digest = [0; 32];
        digest[..body.len().min(32)].copy_from_slice(&body[..body.len().min(32)]);
        digest
    }
    fn describe_key(
        &self,
        key: &[u8],
    ) -> Result<KeyDescription, aseman_domain::identity::AuthenticationError> {
        let (&legacy, name) = key
            .split_first()
            .ok_or(aseman_domain::identity::AuthenticationError::Malformed)?;
        Ok(KeyDescription {
            key_id: format!("zQm{}", String::from_utf8_lossy(name)),
            legacy: legacy == 1,
        })
    }
    fn introduction_bytes(&self, _: &Introduction) -> Vec<u8> {
        unreachable!()
    }
}

impl SessionDirectory for World {
    fn subject(&self, token: &str) -> PortResult<Option<Subject>> {
        Ok(self.sessions.lock().unwrap().get(token).copied())
    }
}

impl ClockPort for World {
    fn unix_millis(&self) -> i64 {
        NOW
    }
}

impl PolicyDecisionPort for World {
    fn decide(&self, _: &PolicyRequest) -> PortResult<PolicyDecision> {
        Ok(PolicyDecision {
            allowed: self.allow,
            reason: if self.allow {
                DecisionReason::Allowed
            } else {
                DecisionReason::ConditionNotMet
            },
            matched: self.allow.then_some(Condition::Authenticated),
            considered: vec![Condition::Authenticated],
            grant_chain: Vec::new(),
            registry_version: "test".to_owned(),
            policy_version: "test-1".to_owned(),
        })
    }
}

impl GrantStore for World {
    fn grant(&self, _: aseman_domain::Uuid) -> PortResult<Option<Grant>> {
        Ok(None)
    }
    fn grants_of(&self, _: &Subject) -> PortResult<Vec<Grant>> {
        Ok(Vec::new())
    }
    fn children(&self, _: aseman_domain::Uuid) -> PortResult<Vec<Grant>> {
        Ok(Vec::new())
    }
    fn put(&self, _: &Grant) -> PortResult<()> {
        unreachable!()
    }
    fn revoke(&self, _: aseman_domain::Uuid, _: i64) -> PortResult<()> {
        unreachable!()
    }
}

impl DecisionAudit for World {
    fn record(&self, record: &aseman_domain::authority::AuditRecord) -> PortResult<u64> {
        let mut audit = self.audit.lock().unwrap();
        audit.push(record.clone());
        Ok(u64::try_from(audit.len()).unwrap())
    }
    fn stream(&self, _: &str) -> PortResult<Vec<aseman_domain::authority::AuditedDecision>> {
        Ok(Vec::new())
    }
}

impl PublicActionIdempotency for World {
    fn claim(&self, subject: &str, key: &str, digest: [u8; 32]) -> PortResult<PublicActionClaim> {
        let mut stored = self.idempotency.lock().unwrap();
        let entry = stored.get(&(subject.to_owned(), key.to_owned()));
        match entry {
            Some((stored_digest, _, _)) if *stored_digest != digest.to_vec() => {
                Ok(PublicActionClaim::Mismatch)
            }
            Some((_, _, false)) => Ok(PublicActionClaim::InProgress),
            Some((_, response, true)) => Ok(PublicActionClaim::Completed(response.clone())),
            None => {
                stored.insert(
                    (subject.to_owned(), key.to_owned()),
                    (digest.to_vec(), Vec::new(), false),
                );
                Ok(PublicActionClaim::Claimed)
            }
        }
    }
    fn complete(&self, subject: &str, key: &str, response: &[u8]) -> PortResult<()> {
        let mut stored = self.idempotency.lock().unwrap();
        let entry = stored.get_mut(&(subject.to_owned(), key.to_owned()));
        match entry {
            Some((_, stored_response, completed)) => {
                *stored_response = response.to_vec();
                *completed = true;
            }
            None => {
                stored.insert(
                    (subject.to_owned(), key.to_owned()),
                    (Vec::new(), response.to_vec(), true),
                );
            }
        }
        Ok(())
    }
    fn release(&self, subject: &str, key: &str) -> PortResult<()> {
        let mut stored = self.idempotency.lock().unwrap();
        let entry = stored.get_mut(&(subject.to_owned(), key.to_owned()));
        match entry {
            Some((_, _, false)) => {
                stored.remove(&(subject.to_owned(), key.to_owned()));
            }
            Some((_, _, true)) => {}
            None => {}
        }
        Ok(())
    }
}

impl ActionExecutor for World {
    fn resolve(
        &self,
        _: &Subject,
        _: &str,
        _: &[u8],
    ) -> PortResult<(ResourceRef, BTreeSet<Condition>)> {
        Ok((
            ResourceRef {
                kind: "creature".to_owned(),
                id: "c".to_owned(),
            },
            BTreeSet::from([Condition::Authenticated]),
        ))
    }
    fn execute(&self, _: Subject, _: &str, _: &[u8]) -> PortResult<Vec<u8>> {
        *self.executed.lock().unwrap() += 1;
        Ok(br#"{"ok":true}"#.to_vec())
    }
}

fn world() -> World {
    let world = World::default();
    world.keys.lock().unwrap().insert(
        "zQmKey".to_owned(),
        IdentityKey {
            key_id: "zQmKey".to_owned(),
            public_key: vec![0, b'K', b'e', b'y'],
            epoch: KeyEpoch {
                subject: subject(),
                purpose: KeyPurpose::Authentication,
                epoch: 1,
                not_before_millis: NOW - 1_000,
                expires_at_millis: None,
                retired_at_millis: None,
                revoked_at_millis: None,
                legacy: false,
            },
        },
    );
    world
}

fn proof(world: &World, action: &str, nonce: u8) -> Proof {
    let mut digest = [0; 32];
    digest[..2].copy_from_slice(b"{}");
    Proof {
        context: SignatureContext::Request,
        algorithm: "ed25519".to_owned(),
        key_id: "zQmKey".to_owned(),
        key_epoch: 1,
        subject: subject(),
        audience: "node:a/public/v1".to_owned(),
        window: CredentialWindow {
            issued_at_millis: NOW,
            not_before_millis: NOW,
            expires_at_millis: NOW + 60_000,
        },
        nonce: vec![nonce; 16],
        request_id: "r".to_owned(),
        action: action.to_owned(),
        resource: "c".to_owned(),
        body_digest: world.body_digest(b"{}"),
        signature: b"good".to_vec(),
    }
}

fn verifier_policy() -> VerifierPolicy {
    VerifierPolicy {
        audience: "node:a/public/v1".to_owned(),
        freshness: FreshnessPolicy::GUEST,
        rotation: RotationPolicy::DEFAULT,
    }
}

fn service(world: Arc<World>) -> ComposedPublicActionService {
    ComposedPublicActionService::new(
        world.clone(),
        world.clone(),
        world.clone(),
        world.clone(),
        world.clone(),
        world.clone(),
        world.clone(),
        world.clone(),
        world.clone(),
        world,
        verifier_policy(),
    )
}

fn request(
    world: &World,
    action: &str,
    proof: &Proof,
    idempotency_key: Option<&str>,
) -> aseman_public_http::PublicActionRequest {
    let _ = world;
    aseman_public_http::PublicActionRequest {
        request_id: "req-1".to_owned(),
        route: "/v1/actions/creatures/update".to_owned(),
        action: action.to_owned(),
        class: if idempotency_key.is_some() {
            ActionClass::Write
        } else {
            ActionClass::Read
        },
        authentication: Authentication::Proof(Box::new(proof.clone())),
        idempotency_key: idempotency_key.map(str::to_owned),
        body: b"{}".to_vec(),
    }
}

#[test]
fn composed_service_serves_an_admitted_action_and_returns_rfc9457_errors() {
    let mut base = world();
    base.allow = true;
    let world = Arc::new(base);
    let service = service(world.clone());
    let signed = proof(&world, "creature.update", 1);
    let response = service
        .invoke(request(&world, "creature.update", &signed, None))
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, br#"{"ok":true}"#.to_vec());
    assert_eq!(*world.executed.lock().unwrap(), 1);
    assert_eq!(world.audit.lock().unwrap().len(), 1);
}

#[test]
fn denied_actions_map_to_a_403_problem() {
    let mut base = world();
    base.allow = false;
    let world = Arc::new(base);
    let service = service(world.clone());
    let signed = proof(&world, "creature.update", 1);
    let error = service
        .invoke(request(&world, "creature.update", &signed, None))
        .unwrap_err();
    assert_eq!(error.status, 403);
    assert_eq!(error.reason, "denied");
    assert_eq!(*world.executed.lock().unwrap(), 0);
    assert_eq!(world.audit.lock().unwrap().len(), 1);
}

#[test]
fn a_mutation_retry_replays_without_reexecuting() {
    let mut base = world();
    base.allow = true;
    let world = Arc::new(base);
    let service = service(world.clone());
    let signed = proof(&world, "creature.update", 1);
    let first = service
        .invoke(request(
            &world,
            "creature.update",
            &signed,
            Some("request-key-0001"),
        ))
        .unwrap();
    assert_eq!(first.body, br#"{"ok":true}"#.to_vec());
    assert_eq!(*world.executed.lock().unwrap(), 1);

    let retry = service
        .invoke(request(
            &world,
            "creature.update",
            &proof(&world, "creature.update", 2),
            Some("request-key-0001"),
        ))
        .unwrap();
    assert_eq!(retry.body, first.body);
    assert_eq!(*world.executed.lock().unwrap(), 1);
}
