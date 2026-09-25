use std::collections::BTreeSet;
use std::sync::Mutex;

use aseman_domain::authority::{
    ActionClass, Condition, DecisionReason, PolicyDecision, PolicyRequest, ResourceRef,
};
use aseman_domain::capability::Grant;
use aseman_domain::identity::{
    AuthenticationError, CredentialWindow, FreshnessPolicy, IdentityKey, Introduction,
    KeyDescription, KeyEpoch, KeyPurpose, RotationPolicy, SignatureContext, Subject, SubjectKind,
};
use aseman_ports::{
    ActionExecutor, ClockPort, DecisionAudit, GrantStore, IdentityVerifier, KeyDirectory,
    PolicyDecisionPort, PortError, PortResult, PublicActionIdempotency, ReplayGuard,
    SessionDirectory,
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
    resolved: Mutex<Vec<(Subject, String, Vec<u8>)>>,
    executed: Mutex<Vec<(Subject, String, Vec<u8>)>>,
    audit: Mutex<Vec<AuditRecord>>,
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
    fn verify(&self, proof: &Proof, _: &IdentityKey) -> Result<(), AuthenticationError> {
        (proof.signature == b"good")
            .then_some(())
            .ok_or(AuthenticationError::BadSignature)
    }
    fn body_digest(&self, body: &[u8]) -> [u8; 32] {
        let mut digest = [0; 32];
        digest[..body.len().min(32)].copy_from_slice(&body[..body.len().min(32)]);
        digest
    }
    fn describe_key(&self, key: &[u8]) -> Result<KeyDescription, AuthenticationError> {
        let (&legacy, name) = key.split_first().ok_or(AuthenticationError::Malformed)?;
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
    fn decide(&self, _request: &PolicyRequest) -> PortResult<PolicyDecision> {
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
    fn record(&self, record: &AuditRecord) -> PortResult<u64> {
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
        subject: &Subject,
        action: &str,
        body: &[u8],
    ) -> PortResult<(ResourceRef, BTreeSet<Condition>)> {
        self.resolved
            .lock()
            .unwrap()
            .push((*subject, action.to_owned(), body.to_vec()));
        Ok((
            ResourceRef {
                kind: "creature".to_owned(),
                id: "c".to_owned(),
            },
            BTreeSet::from([Condition::Authenticated]),
        ))
    }
    fn execute(&self, subject: Subject, action: &str, body: &[u8]) -> PortResult<Vec<u8>> {
        self.executed
            .lock()
            .unwrap()
            .push((subject, action.to_owned(), body.to_vec()));
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
        .sessions
        .lock()
        .unwrap()
        .insert("session-1".to_owned(), subject());
    world
}

fn proof(world: &World, action: &str, resource: &str, nonce: u8) -> Proof {
    let mut digest = [0; 32];
    digest[..4].copy_from_slice(b"body");
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
        resource: resource.to_owned(),
        body_digest: world.body_digest(b"body"),
        signature: b"good".to_vec(),
    }
}

fn policy() -> VerifierPolicy {
    VerifierPolicy {
        audience: "node:a/public/v1".to_owned(),
        freshness: FreshnessPolicy::GUEST,
        rotation: RotationPolicy::DEFAULT,
    }
}

fn service<'a>(world: &'a World, verifier_policy: &'a VerifierPolicy) -> ServePublicAction<'a> {
    ServePublicAction {
        keys: world,
        replay: world,
        sessions: world,
        verifier: world,
        clock: world,
        policy: world,
        grants: world,
        audit: world,
        idempotency: world,
        executor: world,
        verifier_policy,
    }
}

fn request(
    world: &World,
    action: &str,
    authentication: RequestAuthentication,
    idempotency_key: Option<&str>,
) -> PublicActionRequest {
    let _ = world;
    PublicActionRequest {
        request_id: "req-1".to_owned(),
        route: "/v1/actions/creatures/update".to_owned(),
        action: action.to_owned(),
        class: if idempotency_key.is_some() {
            ActionClass::Write
        } else {
            ActionClass::Read
        },
        authentication,
        idempotency_key: idempotency_key.map(str::to_owned),
        body: b"body".to_vec(),
    }
}

#[test]
fn a_signed_request_authenticates_authorizes_and_executes_once() {
    let mut world = world();
    world.allow = true;
    let signed = proof(&world, "creature.update", "c", 1);
    let response = service(&world, &policy())
        .execute(&request(
            &world,
            "creature.update",
            RequestAuthentication::Proof(Box::new(signed)),
            None,
        ))
        .unwrap();
    assert_eq!(response.body, br#"{"ok":true}"#.to_vec());
    assert_eq!(world.executed.lock().unwrap().len(), 1);
    assert_eq!(world.resolved.lock().unwrap().len(), 1);
    let audit = world.audit.lock().unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].decision, "allowed");
    assert_eq!(audit[0].action, "creature.update");
}

#[test]
fn a_session_token_resolves_to_its_subject() {
    let mut world = world();
    world.allow = true;
    let response = service(&world, &policy())
        .execute(&request(
            &world,
            "creature.update",
            RequestAuthentication::Session("session-1".to_owned()),
            None,
        ))
        .unwrap();
    assert_eq!(response.body, br#"{"ok":true}"#.to_vec());
    let audit = world.audit.lock().unwrap();
    assert_eq!(audit[0].actor, subject().to_string());
}

#[test]
fn unknown_sessions_and_misbound_proofs_are_refused_before_authorization() {
    let mut world = world();
    world.allow = true;
    let unknown = service(&world, &policy())
        .execute(&request(
            &world,
            "creature.update",
            RequestAuthentication::Session("session-missing".to_owned()),
            None,
        ))
        .unwrap_err();
    assert_eq!(unknown, PublicActionFailure::Refused("unknown session"));

    let misbound = proof(&world, "creature.delete", "c", 2);
    let refused = service(&world, &policy())
        .execute(&request(
            &world,
            "creature.update",
            RequestAuthentication::Proof(Box::new(misbound)),
            None,
        ))
        .unwrap_err();
    assert_eq!(
        refused,
        PublicActionFailure::Refused("the credential does not cover this action")
    );
    assert!(world.executed.lock().unwrap().is_empty());
    assert!(world.audit.lock().unwrap().is_empty());
}

#[test]
fn a_denied_action_never_runs_but_is_audited() {
    let mut world = world();
    world.allow = false;
    let signed = proof(&world, "creature.update", "c", 1);
    let failure = service(&world, &policy())
        .execute(&request(
            &world,
            "creature.update",
            RequestAuthentication::Proof(Box::new(signed)),
            None,
        ))
        .unwrap_err();
    assert_eq!(
        failure,
        PublicActionFailure::Denied("condition_not_met".to_owned())
    );
    assert!(world.executed.lock().unwrap().is_empty());
    let audit = world.audit.lock().unwrap();
    assert_eq!(audit[0].decision, "condition_not_met");
}

#[test]
fn a_replayed_proof_is_refused_by_a401() {
    let mut world = world();
    world.allow = true;
    let policy = policy();
    let service = service(&world, &policy);
    let signed = proof(&world, "creature.update", "c", 1);
    let first = request(
        &world,
        "creature.update",
        RequestAuthentication::Proof(Box::new(signed)),
        None,
    );
    assert!(service.execute(&first).is_ok());
    assert_eq!(
        service.execute(&first).unwrap_err(),
        PublicActionFailure::Rejected(AuthenticationError::Replayed)
    );
}

#[test]
fn a_mutation_runs_once_under_its_key_and_replays_the_outcome() {
    let mut world = world();
    world.allow = true;
    let policy = policy();
    let service = service(&world, &policy);
    let key = "request-key-0001";
    let first_proof = proof(&world, "creature.update", "c", 1);
    let first = request(
        &world,
        "creature.update",
        RequestAuthentication::Proof(Box::new(first_proof)),
        Some(key),
    );
    let first_response = service.execute(&first).unwrap();
    assert_eq!(first_response.body, br#"{"ok":true}"#.to_vec());
    // The same key replays the recorded outcome without executing again.
    let retry_proof = proof(&world, "creature.update", "c", 2);
    let retry = request(
        &world,
        "creature.update",
        RequestAuthentication::Proof(Box::new(retry_proof)),
        Some(key),
    );
    let replayed = service.execute(&retry).unwrap();
    assert_eq!(replayed.body, first_response.body);
    assert_eq!(world.executed.lock().unwrap().len(), 1);
    assert_eq!(world.audit.lock().unwrap().len(), 2);
}

#[test]
fn a_mismatched_or_inflight_mutation_is_refused_without_running() {
    let mut world = world();
    world.allow = true;
    let policy = policy();
    let service = service(&world, &policy);
    let owner = subject().to_string();
    // The digest the mutation will compute: SHA-256 of `action \0 body`.
    let mut action_body = b"creature.update".to_vec();
    action_body.push(0);
    action_body.extend_from_slice(b"body");
    let request_digest = world.body_digest(&action_body);
    // A completed key under a digest that differs from the next request's.
    world.idempotency.lock().unwrap().insert(
        (owner.clone(), "key-completed".to_owned()),
        (vec![9; 32], br#"{"ok":true}"#.to_vec(), true),
    );
    // An unfinished key is in progress.
    world.idempotency.lock().unwrap().insert(
        (owner.clone(), "key-inflight".to_owned()),
        (request_digest.to_vec(), Vec::new(), false),
    );

    // A different request (a different body) under the completed key is a mismatch,
    // not a replay. The proof signs that body, so authentication passes first.
    let mut mismatch_proof = proof(&world, "creature.update", "c", 3);
    mismatch_proof.body_digest = world.body_digest(b"different");
    let mismatch = PublicActionRequest {
        request_id: "req-2".to_owned(),
        route: "/v1/actions/creatures/update".to_owned(),
        action: "creature.update".to_owned(),
        class: ActionClass::Write,
        authentication: RequestAuthentication::Proof(Box::new(mismatch_proof)),
        idempotency_key: Some("key-completed".to_owned()),
        body: b"different".to_vec(),
    };
    assert_eq!(
        service.execute(&mismatch).unwrap_err(),
        PublicActionFailure::IdempotencyMismatch
    );
    // An unfinished key is in progress.
    let inflight_proof = proof(&world, "creature.update", "c", 4);
    let inflight = request(
        &world,
        "creature.update",
        RequestAuthentication::Proof(Box::new(inflight_proof)),
        Some("key-inflight"),
    );
    assert_eq!(
        service.execute(&inflight).unwrap_err(),
        PublicActionFailure::IdempotencyInProgress
    );
    assert_eq!(world.executed.lock().unwrap().len(), 0);
}

#[test]
fn reads_do_not_need_an_idempotency_key() {
    let mut world = world();
    world.allow = true;
    let signed = proof(&world, "creature.read", "c", 1);
    let response = service(&world, &policy())
        .execute(&request(
            &world,
            "creature.read",
            RequestAuthentication::Proof(Box::new(signed)),
            None,
        ))
        .unwrap();
    assert_eq!(response.body, br#"{"ok":true}"#.to_vec());
    assert!(world.idempotency.lock().unwrap().is_empty());
}

#[test]
fn the_typed_action_class_controls_idempotency_at_the_use_case_boundary() {
    let mut world = world();
    world.allow = true;
    let policy = policy();
    let service = service(&world, &policy);

    let mut read = request(
        &world,
        "creature.read",
        RequestAuthentication::Proof(Box::new(proof(&world, "creature.read", "c", 1))),
        None,
    );
    // An optional header on a read must not turn it into a mutation.
    read.idempotency_key = Some("request-key-0001".to_owned());
    assert!(service.execute(&read).is_ok());
    assert!(world.idempotency.lock().unwrap().is_empty());

    let mut mutation = request(
        &world,
        "creature.update",
        RequestAuthentication::Proof(Box::new(proof(&world, "creature.update", "c", 2))),
        None,
    );
    mutation.class = ActionClass::Write;
    assert_eq!(
        service.execute(&mutation).unwrap_err(),
        PublicActionFailure::Refused("idempotency key required")
    );
    assert_eq!(world.executed.lock().unwrap().len(), 1);
}

/// A deterministic load-lite property: across many interleaved mutations with
/// distinct keys, exactly one executes per key and every retry replays the first
/// outcome. This is the property the durable store must preserve under load; it is
/// written deterministically (seeded iteration, in-process store) so it runs in the
/// fast gate, mirroring the A403 property-test style.
#[test]
fn many_mutations_run_once_each_and_replay_their_first_outcome() {
    let mut world = world();
    world.allow = true;
    let policy = policy();
    let service = service(&world, &policy);
    // A deterministic schedule of mutations over 50 distinct keys: first a fresh
    // attempt, then a retry, occasionally a third replay, all in one sequence. Every
    // request gets a globally distinct nonce so A401 accepts it; only the idempotency
    // key binds the retries.
    let mut nonce = 0u8;
    for round in 0..50u8 {
        let key = format!("round-{round}");
        let first = request(
            &world,
            "creature.update",
            RequestAuthentication::Proof(Box::new(proof(&world, "creature.update", "c", nonce))),
            Some(&key),
        );
        nonce = nonce.wrapping_add(1);
        let first_response = service.execute(&first).unwrap();
        assert_eq!(first_response.body, br#"{"ok":true}"#.to_vec());
        for replay in 1..=(round % 3) {
            let retry = request(
                &world,
                "creature.update",
                RequestAuthentication::Proof(Box::new(proof(
                    &world,
                    "creature.update",
                    "c",
                    nonce,
                ))),
                Some(&key),
            );
            nonce = nonce.wrapping_add(1);
            let replayed = service.execute(&retry).unwrap();
            assert_eq!(
                replayed.body, first_response.body,
                "round {round} replay {replay}"
            );
        }
    }
    // Each key executed exactly once, whatever the replays.
    assert_eq!(world.executed.lock().unwrap().len(), 50);
    // And the store recorded exactly one completed outcome per key.
    let completed = world
        .idempotency
        .lock()
        .unwrap()
        .values()
        .filter(|(_, _, done)| *done)
        .count();
    assert_eq!(completed, 50);
}
