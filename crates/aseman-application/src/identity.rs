//! Proof authentication (A401 section 5). The transport parses the proof (steps 1-3);
//! this use case applies steps 4-12 in the contract's order and returns the
//! authenticated subject. The nonce is recorded only after the signature verifies.

use aseman_domain::identity::{
    AuthenticationError, Challenge, FreshnessPolicy, IdentityKey, Introduction, KeyDescription,
    KeyEpoch, KeyPurpose, MAX_GUEST_CREDENTIAL_MILLIS, Proof, RotationPolicy, SignatureContext,
    Subject, SubjectKind, accept_epoch,
};
use aseman_ports::{
    ChallengeStore, ClockPort, IdentityVerifier, KeyDirectory, PortError, ReplayGuard,
};
use thiserror::Error;

/// Why an identity operation did not happen.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum IdentityFailure {
    /// A proof is refused with an A401 code.
    #[error("authentication failed: {}", .0.code())]
    Rejected(AuthenticationError),
    /// The request is well authenticated but not allowed.
    #[error("refused: {0}")]
    Refused(&'static str),
    /// A directory or store could not answer. Nothing was accepted.
    #[error(transparent)]
    Unavailable(#[from] PortError),
}

impl From<AuthenticationError> for IdentityFailure {
    fn from(error: AuthenticationError) -> Self {
        Self::Rejected(error)
    }
}

/// What a verifier accepts: its own audience and its time rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifierPolicy {
    pub audience: String,
    pub freshness: FreshnessPolicy,
    pub rotation: RotationPolicy,
}

pub struct AuthenticateProof<'a> {
    pub keys: &'a dyn KeyDirectory,
    pub replay: &'a dyn ReplayGuard,
    pub verifier: &'a dyn IdentityVerifier,
    pub clock: &'a dyn ClockPort,
}

impl AuthenticateProof<'_> {
    /// Authenticate `proof` for a request whose body is `body`.
    ///
    /// # Errors
    ///
    /// The first failing A401 check as `Rejected`, or `Unavailable`.
    pub fn execute(
        &self,
        proof: &Proof,
        body: &[u8],
        policy: &VerifierPolicy,
    ) -> Result<Subject, IdentityFailure> {
        let now = self.clock.unix_millis();
        // 4. The named key.
        let key = self
            .keys
            .key(&proof.key_id)?
            .ok_or(AuthenticationError::UnknownKey)?;
        // 5. It belongs to the subject, as the epoch the proof claims.
        if key.epoch.subject != proof.subject {
            return Err(AuthenticationError::KeySubjectMismatch.into());
        }
        if key.epoch.epoch != proof.key_epoch {
            return Err(AuthenticationError::EpochNotAccepted.into());
        }
        // 6-7. Purpose, revocation, legacy limits, validity, and rotation.
        let epochs = self
            .keys
            .epochs(&proof.subject, key.epoch.purpose)?
            .into_iter()
            .map(|stored| stored.epoch)
            .collect::<Vec<_>>();
        accept_epoch(
            &epochs,
            proof.key_epoch,
            proof.context,
            now,
            policy.rotation,
        )?;
        // 8. Audience.
        if proof.audience != policy.audience {
            return Err(AuthenticationError::AudienceMismatch.into());
        }
        // 9. Freshness.
        proof.window.check(now, policy.freshness)?;
        // 10. Body digest.
        if self.verifier.body_digest(body) != proof.body_digest {
            return Err(AuthenticationError::BodyDigestMismatch.into());
        }
        // 11. Signature.
        self.verifier.verify(proof, &key)?;
        // 12. Replay, last: only a signed proof can consume its nonce.
        let retain_until = proof.window.replay_retention_until(policy.freshness);
        if !self
            .replay
            .record_nonce(&proof.key_id, &proof.nonce, retain_until, now)?
        {
            return Err(AuthenticationError::Replayed.into());
        }
        Ok(proof.subject)
    }
}

/// A key a subject asks to register.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewKey {
    /// The A401 versioned encoding.
    pub public_key: Vec<u8>,
    pub expires_at_millis: Option<i64>,
}

fn canonical(
    verifier: &dyn IdentityVerifier,
    key: &NewKey,
) -> Result<KeyDescription, IdentityFailure> {
    let description = verifier.describe_key(&key.public_key)?;
    if description.legacy {
        return Err(IdentityFailure::Refused(
            "new identities use Ed25519 keys (ADR 0009)",
        ));
    }
    Ok(description)
}

fn epoch_record(
    description: KeyDescription,
    key: &NewKey,
    subject: Subject,
    purpose: KeyPurpose,
    epoch: u32,
    now: i64,
) -> IdentityKey {
    IdentityKey {
        key_id: description.key_id,
        public_key: key.public_key.clone(),
        epoch: KeyEpoch {
            subject,
            purpose,
            epoch,
            not_before_millis: now,
            expires_at_millis: key.expires_at_millis,
            retired_at_millis: None,
            revoked_at_millis: None,
            legacy: description.legacy,
        },
    }
}

fn register(keys: &dyn KeyDirectory, key: &IdentityKey) -> Result<(), IdentityFailure> {
    match keys.register(key) {
        Err(PortError::Conflict) => Err(IdentityFailure::Refused("the key is already registered")),
        other => Ok(other?),
    }
}

/// Register the next epoch of an authenticated subject's key and retire the current
/// one; the prior epoch keeps verifying for the overlap (A401 section 6). The caller
/// must have authenticated `subject` (usually with its current key).
pub struct RotateKey<'a> {
    pub keys: &'a dyn KeyDirectory,
    pub verifier: &'a dyn IdentityVerifier,
    pub clock: &'a dyn ClockPort,
}

impl RotateKey<'_> {
    /// # Errors
    ///
    /// `Refused` for a legacy or already registered key, `Rejected` for a malformed
    /// key, or `Unavailable`.
    pub fn execute(
        &self,
        subject: Subject,
        purpose: KeyPurpose,
        key: &NewKey,
    ) -> Result<IdentityKey, IdentityFailure> {
        let now = self.clock.unix_millis();
        let description = canonical(self.verifier, key)?;
        let existing = self.keys.epochs(&subject, purpose)?;
        let next = existing
            .iter()
            .map(|stored| stored.epoch.epoch)
            .max()
            .map_or(1, |latest| latest.saturating_add(1));
        let record = epoch_record(description, key, subject, purpose, next, now);
        register(self.keys, &record)?;
        // Registered first, so a refused key retires nothing.
        for stored in existing
            .iter()
            .filter(|stored| stored.epoch.retired_at_millis.is_none())
        {
            self.keys.retire(&stored.key_id, now)?;
        }
        Ok(record)
    }
}

/// Revoke a key at once; revocation overrides every overlap (A401 section 7). The
/// caller authorizes the revocation (A402).
pub struct RevokeKey<'a> {
    pub keys: &'a dyn KeyDirectory,
    pub clock: &'a dyn ClockPort,
}

impl RevokeKey<'_> {
    /// # Errors
    ///
    /// `Refused` for an unknown key, or `Unavailable`.
    pub fn execute(&self, key_id: &str) -> Result<(), IdentityFailure> {
        match self.keys.revoke(key_id, self.clock.unix_millis()) {
            Err(PortError::NotFound) => Err(IdentityFailure::Refused("unknown key")),
            other => Ok(other?),
        }
    }
}

/// Issue a one-time challenge (A401 "Replay"), valid at most for the guest
/// credential ceiling.
pub struct IssueChallenge<'a> {
    pub challenges: &'a dyn ChallengeStore,
    pub clock: &'a dyn ClockPort,
}

impl IssueChallenge<'_> {
    /// # Errors
    ///
    /// `Refused` for a lifetime outside (0, five minutes], or `Unavailable`.
    pub fn execute(
        &self,
        subject: Subject,
        audience: &str,
        lifetime_millis: i64,
    ) -> Result<Challenge, IdentityFailure> {
        if !(1..=MAX_GUEST_CREDENTIAL_MILLIS).contains(&lifetime_millis) {
            return Err(IdentityFailure::Refused("challenge lifetime out of range"));
        }
        Ok(self.challenges.issue(
            &subject,
            audience,
            self.clock.unix_millis().saturating_add(lifetime_millis),
        )?)
    }
}

/// A legacy identity as the node records it: its typed subject and its legacy public
/// key in the A401 encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyIdentity {
    pub subject: Subject,
    pub public_key: Vec<u8>,
}

/// ADR 0019 / A401 section 10: a legacy identity proves possession of its legacy key by
/// answering a server challenge, and enrolls its first canonical key. The proof's body
/// is the new key's encoding, so the signature binds the key being enrolled. The legacy
/// key is recorded as epoch 0, already retired, so it verifies only through the
/// rotation overlap and never signs authority.
pub struct EnrollCanonicalKey<'a> {
    pub keys: &'a dyn KeyDirectory,
    pub challenges: &'a dyn ChallengeStore,
    pub verifier: &'a dyn IdentityVerifier,
    pub clock: &'a dyn ClockPort,
}

impl EnrollCanonicalKey<'_> {
    /// # Errors
    ///
    /// `Rejected` with the A401 code of a failing proof, `Refused` when the identity is
    /// not legacy, is already enrolled, or offers a legacy key, or `Unavailable`.
    pub fn execute(
        &self,
        legacy: &LegacyIdentity,
        proof: &Proof,
        key: &NewKey,
        policy: &VerifierPolicy,
    ) -> Result<IdentityKey, IdentityFailure> {
        let now = self.clock.unix_millis();
        let legacy_description = self.verifier.describe_key(&legacy.public_key)?;
        if !legacy_description.legacy {
            return Err(IdentityFailure::Refused("the identity has no legacy key"));
        }
        if proof.context != SignatureContext::Challenge {
            return Err(AuthenticationError::KeyPurposeMismatch.into());
        }
        if proof.key_id != legacy_description.key_id {
            return Err(AuthenticationError::UnknownKey.into());
        }
        if proof.subject != legacy.subject {
            return Err(AuthenticationError::KeySubjectMismatch.into());
        }
        if proof.key_epoch != 0 {
            return Err(AuthenticationError::EpochNotAccepted.into());
        }
        if proof.audience != policy.audience {
            return Err(AuthenticationError::AudienceMismatch.into());
        }
        proof.window.check(now, policy.freshness)?;
        if self.verifier.body_digest(&key.public_key) != proof.body_digest {
            return Err(AuthenticationError::BodyDigestMismatch.into());
        }
        let legacy_key = IdentityKey {
            key_id: legacy_description.key_id,
            public_key: legacy.public_key.clone(),
            epoch: KeyEpoch {
                subject: legacy.subject,
                purpose: KeyPurpose::Authentication,
                epoch: 0,
                not_before_millis: now,
                expires_at_millis: None,
                retired_at_millis: Some(now),
                revoked_at_millis: None,
                legacy: true,
            },
        };
        self.verifier.verify(proof, &legacy_key)?;
        if !self
            .challenges
            .consume(&proof.nonce, &legacy.subject, &proof.audience, now)?
        {
            return Err(AuthenticationError::Replayed.into());
        }
        let description = canonical(self.verifier, key)?;
        let existing = self
            .keys
            .epochs(&legacy.subject, KeyPurpose::Authentication)?;
        if existing.iter().any(|stored| !stored.epoch.legacy) {
            return Err(IdentityFailure::Refused(
                "the identity already has a canonical key; rotate it instead",
            ));
        }
        if existing.is_empty() {
            register(self.keys, &legacy_key)?;
        }
        let record = epoch_record(
            description,
            key,
            legacy.subject,
            KeyPurpose::Authentication,
            1,
            now,
        );
        register(self.keys, &record)?;
        Ok(record)
    }
}

/// A401 section 8: an enrolled federation root introduces a node by signing its
/// descriptor key. Roots are the subject's `introduction` keys, which only an
/// administrator registers (with [`RotateKey`]); an introduction only ever registers a
/// `descriptor` key, so introduced nodes can never introduce others.
pub struct AcceptIntroduction<'a> {
    pub keys: &'a dyn KeyDirectory,
    pub replay: &'a dyn ReplayGuard,
    pub verifier: &'a dyn IdentityVerifier,
    pub clock: &'a dyn ClockPort,
}

impl AcceptIntroduction<'_> {
    /// Authenticate the root's `introduction` proof over the canonical introduction
    /// bytes, then register the introduced descriptor key. Returns the introducing root.
    ///
    /// # Errors
    ///
    /// `Rejected` with the A401 code of a failing proof, `Refused` for a non-node root
    /// or subject, a legacy key, or a key already registered, or `Unavailable`.
    pub fn execute(
        &self,
        proof: &Proof,
        introduction: &Introduction,
        policy: &VerifierPolicy,
    ) -> Result<Subject, IdentityFailure> {
        if proof.context != SignatureContext::Introduction {
            return Err(AuthenticationError::KeyPurposeMismatch.into());
        }
        let body = self.verifier.introduction_bytes(introduction);
        let root = AuthenticateProof {
            keys: self.keys,
            replay: self.replay,
            verifier: self.verifier,
            clock: self.clock,
        }
        .execute(proof, &body, policy)?;
        if root.kind != SubjectKind::Node || introduction.subject.kind != SubjectKind::Node {
            return Err(IdentityFailure::Refused("introductions are between nodes"));
        }
        let description = self.verifier.describe_key(&introduction.public_key)?;
        if description.legacy {
            return Err(IdentityFailure::Refused(
                "new identities use Ed25519 keys (ADR 0009)",
            ));
        }
        register(
            self.keys,
            &IdentityKey {
                key_id: description.key_id,
                public_key: introduction.public_key.clone(),
                epoch: KeyEpoch {
                    subject: introduction.subject,
                    purpose: KeyPurpose::Descriptor,
                    epoch: introduction.epoch,
                    not_before_millis: introduction.not_before_millis,
                    expires_at_millis: introduction.expires_at_millis,
                    retired_at_millis: None,
                    revoked_at_millis: None,
                    legacy: false,
                },
            },
        )?;
        Ok(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_domain::identity::CredentialWindow;
    use aseman_ports::PortResult;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    const NOW: i64 = 1_800_000_000_000;

    fn subject(kind: SubjectKind) -> Subject {
        Subject {
            kind,
            id: "0190f1a2-7b3c-7d4e-8f00-112233445566".parse().unwrap(),
        }
    }

    /// A directory, replay store, clock, and a verifier that accepts the signature
    /// `b"good"`, with replay calls counted.
    #[derive(Default)]
    struct World {
        keys: Mutex<BTreeMap<String, IdentityKey>>,
        nonces: Mutex<BTreeSet<Vec<u8>>>,
        replay_calls: Mutex<usize>,
        challenges: Mutex<Vec<Challenge>>,
        unavailable: bool,
    }

    impl KeyDirectory for World {
        fn key(&self, key_id: &str) -> PortResult<Option<IdentityKey>> {
            if self.unavailable {
                return Err(PortError::Failed("down".to_owned()));
            }
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
        fn retire(&self, key_id: &str, at: i64) -> PortResult<()> {
            let mut keys = self.keys.lock().unwrap();
            let key = keys.get_mut(key_id).ok_or(PortError::NotFound)?;
            let retired = key.epoch.retired_at_millis.get_or_insert(at);
            *retired = (*retired).min(at);
            Ok(())
        }
        fn revoke(&self, key_id: &str, at: i64) -> PortResult<()> {
            let mut keys = self.keys.lock().unwrap();
            let key = keys.get_mut(key_id).ok_or(PortError::NotFound)?;
            let revoked = key.epoch.revoked_at_millis.get_or_insert(at);
            *revoked = (*revoked).min(at);
            Ok(())
        }
    }

    impl ReplayGuard for World {
        fn record_nonce(&self, _: &str, nonce: &[u8], _: i64, _: i64) -> PortResult<bool> {
            *self.replay_calls.lock().unwrap() += 1;
            Ok(self.nonces.lock().unwrap().insert(nonce.to_vec()))
        }
    }

    impl IdentityVerifier for World {
        fn verify(&self, proof: &Proof, _: &IdentityKey) -> Result<(), AuthenticationError> {
            if proof.signature == b"good" {
                Ok(())
            } else {
                Err(AuthenticationError::BadSignature)
            }
        }
        fn body_digest(&self, body: &[u8]) -> [u8; 32] {
            let mut digest = [0; 32];
            digest[0] = u8::try_from(body.len()).unwrap();
            digest
        }
        /// Test keys: `[legacy flag, name...]`, named `zQm{name}`.
        fn describe_key(&self, key: &[u8]) -> Result<KeyDescription, AuthenticationError> {
            let (&legacy, name) = key.split_first().ok_or(AuthenticationError::Malformed)?;
            Ok(KeyDescription {
                key_id: format!("zQm{}", String::from_utf8_lossy(name)),
                legacy: legacy == 1,
            })
        }
        fn introduction_bytes(&self, introduction: &Introduction) -> Vec<u8> {
            // Four bytes, so the test digest (the body length) is 4 like `proof()`'s.
            let mut bytes = introduction.subject.id.as_bytes()[..3].to_vec();
            bytes.push(u8::try_from(introduction.epoch).unwrap());
            bytes
        }
    }

    impl ChallengeStore for World {
        fn issue(&self, subject: &Subject, audience: &str, expires: i64) -> PortResult<Challenge> {
            let mut challenges = self.challenges.lock().unwrap();
            let challenge = Challenge {
                nonce: vec![u8::try_from(challenges.len()).unwrap() + 100; 32],
                subject: *subject,
                audience: audience.to_owned(),
                expires_at_millis: expires,
            };
            challenges.push(challenge.clone());
            Ok(challenge)
        }
        fn consume(
            &self,
            nonce: &[u8],
            subject: &Subject,
            audience: &str,
            now: i64,
        ) -> PortResult<bool> {
            let mut challenges = self.challenges.lock().unwrap();
            let Some(index) = challenges.iter().position(|challenge| {
                challenge.nonce == nonce
                    && challenge.subject == *subject
                    && challenge.audience == audience
                    && now < challenge.expires_at_millis
            }) else {
                return Ok(false);
            };
            challenges.remove(index);
            Ok(true)
        }
    }

    impl ClockPort for World {
        fn unix_millis(&self) -> i64 {
            NOW
        }
    }

    fn world() -> World {
        let world = World::default();
        world.keys.lock().unwrap().insert(
            "zQmKey".to_owned(),
            IdentityKey {
                key_id: "zQmKey".to_owned(),
                public_key: Vec::new(),
                epoch: KeyEpoch {
                    subject: subject(SubjectKind::Workload),
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

    fn proof() -> Proof {
        let mut digest = [0; 32];
        digest[0] = 4;
        Proof {
            context: SignatureContext::Request,
            algorithm: "ed25519".to_owned(),
            key_id: "zQmKey".to_owned(),
            key_epoch: 1,
            subject: subject(SubjectKind::Workload),
            audience: "node:a/guest/v1".to_owned(),
            window: CredentialWindow {
                issued_at_millis: NOW,
                not_before_millis: NOW,
                expires_at_millis: NOW + 60_000,
            },
            nonce: vec![7; 16],
            request_id: "r".to_owned(),
            action: "guest.kv.get".to_owned(),
            resource: String::new(),
            body_digest: digest,
            signature: b"good".to_vec(),
        }
    }

    fn policy() -> VerifierPolicy {
        VerifierPolicy {
            audience: "node:a/guest/v1".to_owned(),
            freshness: FreshnessPolicy::GUEST,
            rotation: RotationPolicy::DEFAULT,
        }
    }

    fn run(world: &World, proof: &Proof) -> Result<Subject, IdentityFailure> {
        AuthenticateProof {
            keys: world,
            replay: world,
            verifier: world,
            clock: world,
        }
        .execute(proof, b"body", &policy())
    }

    #[test]
    fn a_valid_proof_authenticates_once() {
        let world = world();
        assert_eq!(run(&world, &proof()), Ok(subject(SubjectKind::Workload)));
        assert_eq!(
            run(&world, &proof()),
            Err(AuthenticationError::Replayed.into())
        );
    }

    #[test]
    fn each_check_rejects_with_its_code_and_never_consumes_the_nonce() {
        let world = world();
        type Case = (fn(&mut Proof), AuthenticationError);
        let cases: Vec<Case> = vec![
            (
                |p| p.key_id = "zQmNone".into(),
                AuthenticationError::UnknownKey,
            ),
            (
                |p| p.subject.kind = SubjectKind::Creature,
                AuthenticationError::KeySubjectMismatch,
            ),
            (|p| p.key_epoch = 2, AuthenticationError::EpochNotAccepted),
            (
                |p| p.context = SignatureContext::Token,
                AuthenticationError::KeyPurposeMismatch,
            ),
            (
                |p| p.audience = "node:b".into(),
                AuthenticationError::AudienceMismatch,
            ),
            (
                |p| p.window.expires_at_millis = NOW - 60_000,
                AuthenticationError::Malformed,
            ),
            (
                |p| {
                    p.window.issued_at_millis = NOW - 120_000;
                    p.window.not_before_millis = NOW - 120_000;
                    p.window.expires_at_millis = NOW - 60_000;
                },
                AuthenticationError::Expired,
            ),
            (
                |p| p.body_digest[0] = 9,
                AuthenticationError::BodyDigestMismatch,
            ),
            (
                |p| p.signature = b"forged".to_vec(),
                AuthenticationError::BadSignature,
            ),
        ];
        for (change, expected) in cases {
            let mut changed = proof();
            change(&mut changed);
            assert_eq!(run(&world, &changed), Err(expected.into()), "{expected:?}");
        }
        assert_eq!(*world.replay_calls.lock().unwrap(), 0);
        // The untouched proof still authenticates: nothing burned its nonce.
        assert!(run(&world, &proof()).is_ok());
    }

    #[test]
    fn revoked_keys_and_unavailable_directories_are_refused() {
        let revoked = world();
        revoked
            .keys
            .lock()
            .unwrap()
            .get_mut("zQmKey")
            .unwrap()
            .epoch
            .revoked_at_millis = Some(NOW - 1);
        assert_eq!(
            run(&revoked, &proof()),
            Err(AuthenticationError::RevokedKey.into())
        );
        let down = World {
            unavailable: true,
            ..world()
        };
        assert!(matches!(
            run(&down, &proof()),
            Err(IdentityFailure::Unavailable(_))
        ));
    }
    fn new_key(legacy: bool, name: &str) -> NewKey {
        let mut public_key = vec![u8::from(legacy)];
        public_key.extend_from_slice(name.as_bytes());
        NewKey {
            public_key,
            expires_at_millis: None,
        }
    }

    #[test]
    fn rotation_registers_the_next_epoch_and_retires_the_current_one() {
        let world = world();
        let rotate = RotateKey {
            keys: &world,
            verifier: &world,
            clock: &world,
        };
        let workload = subject(SubjectKind::Workload);
        let second = rotate
            .execute(workload, KeyPurpose::Authentication, &new_key(false, "Two"))
            .unwrap();
        assert_eq!((second.key_id.as_str(), second.epoch.epoch), ("zQmTwo", 2));
        let epochs = world.epochs(&workload, KeyPurpose::Authentication).unwrap();
        assert_eq!(epochs[0].epoch.retired_at_millis, Some(NOW));
        assert_eq!(epochs[1].epoch.retired_at_millis, None);
        // Legacy keys never become new identities, and a key registers once.
        assert_eq!(
            rotate.execute(workload, KeyPurpose::Authentication, &new_key(true, "Rsa")),
            Err(IdentityFailure::Refused(
                "new identities use Ed25519 keys (ADR 0009)"
            ))
        );
        assert_eq!(
            rotate.execute(workload, KeyPurpose::Authentication, &new_key(false, "Two")),
            Err(IdentityFailure::Refused("the key is already registered"))
        );
        // A refused key retired nothing.
        assert_eq!(
            world
                .key("zQmTwo")
                .unwrap()
                .unwrap()
                .epoch
                .retired_at_millis,
            None
        );
        // A subject's first key is epoch 1.
        let first = rotate
            .execute(
                subject(SubjectKind::Node),
                KeyPurpose::Descriptor,
                &new_key(false, "Node"),
            )
            .unwrap();
        assert_eq!(first.epoch.epoch, 1);

        let revoke = RevokeKey {
            keys: &world,
            clock: &world,
        };
        revoke.execute("zQmTwo").unwrap();
        assert_eq!(
            world
                .key("zQmTwo")
                .unwrap()
                .unwrap()
                .epoch
                .revoked_at_millis,
            Some(NOW)
        );
        assert_eq!(
            revoke.execute("zQmNone"),
            Err(IdentityFailure::Refused("unknown key"))
        );
    }

    #[test]
    fn legacy_identities_enroll_once_by_answering_a_challenge_with_the_legacy_key() {
        let world = World::default();
        let creature = subject(SubjectKind::Creature);
        let challenge = IssueChallenge {
            challenges: &world,
            clock: &world,
        }
        .execute(creature, "node:a/guest/v1", 60_000)
        .unwrap();
        let legacy = LegacyIdentity {
            subject: creature,
            public_key: new_key(true, "Legacy").public_key,
        };
        let canonical_key = new_key(false, "Ed");
        let answer = Proof {
            context: SignatureContext::Challenge,
            key_id: "zQmLegacy".to_owned(),
            key_epoch: 0,
            subject: creature,
            nonce: challenge.nonce.clone(),
            body_digest: world.body_digest(&canonical_key.public_key),
            algorithm: "rsa-pss-sha256".to_owned(),
            ..proof()
        };
        let enroll = EnrollCanonicalKey {
            keys: &world,
            challenges: &world,
            verifier: &world,
            clock: &world,
        };
        // A proof that does not bind this key, or is not a challenge answer, fails.
        assert_eq!(
            enroll.execute(&legacy, &answer, &new_key(false, "Other"), &policy()),
            Err(AuthenticationError::BodyDigestMismatch.into())
        );
        let request = Proof {
            context: SignatureContext::Request,
            ..answer.clone()
        };
        assert_eq!(
            enroll.execute(&legacy, &request, &canonical_key, &policy()),
            Err(AuthenticationError::KeyPurposeMismatch.into())
        );
        let forged = Proof {
            signature: b"forged".to_vec(),
            ..answer.clone()
        };
        assert_eq!(
            enroll.execute(&legacy, &forged, &canonical_key, &policy()),
            Err(AuthenticationError::BadSignature.into())
        );
        // None of those consumed the challenge.
        let enrolled = enroll
            .execute(&legacy, &answer, &canonical_key, &policy())
            .unwrap();
        assert_eq!(
            (enrolled.key_id.as_str(), enrolled.epoch.epoch),
            ("zQmEd", 1)
        );
        let epochs = world.epochs(&creature, KeyPurpose::Authentication).unwrap();
        assert_eq!(epochs.len(), 2);
        assert!(epochs[0].epoch.legacy);
        assert_eq!(epochs[0].epoch.retired_at_millis, Some(NOW));
        // The challenge is spent, and enrollment happens once.
        assert_eq!(
            enroll.execute(&legacy, &answer, &canonical_key, &policy()),
            Err(AuthenticationError::Replayed.into())
        );
        let second = IssueChallenge {
            challenges: &world,
            clock: &world,
        }
        .execute(creature, "node:a/guest/v1", 60_000)
        .unwrap();
        assert_eq!(
            enroll.execute(
                &legacy,
                &Proof {
                    nonce: second.nonce,
                    ..answer
                },
                &canonical_key,
                &policy()
            ),
            Err(IdentityFailure::Refused(
                "the identity already has a canonical key; rotate it instead"
            ))
        );
        assert_eq!(
            IssueChallenge {
                challenges: &world,
                clock: &world,
            }
            .execute(creature, "node:a", MAX_GUEST_CREDENTIAL_MILLIS + 1),
            Err(IdentityFailure::Refused("challenge lifetime out of range"))
        );
    }
    #[test]
    fn enrolled_roots_introduce_descriptor_keys_that_cannot_introduce() {
        let world = World::default();
        let root = subject(SubjectKind::Node);
        RotateKey {
            keys: &world,
            verifier: &world,
            clock: &world,
        }
        .execute(root, KeyPurpose::Introduction, &new_key(false, "Root"))
        .unwrap();
        let introduced = Subject {
            kind: SubjectKind::Node,
            id: "0190f1a2-7b3c-7d4e-8f00-00000000beef".parse().unwrap(),
        };
        let introduction = Introduction {
            subject: introduced,
            public_key: new_key(false, "Peer").public_key,
            epoch: 1,
            not_before_millis: NOW,
            expires_at_millis: None,
        };
        let answer = Proof {
            context: SignatureContext::Introduction,
            key_id: "zQmRoot".to_owned(),
            subject: root,
            ..proof()
        };
        let accept = AcceptIntroduction {
            keys: &world,
            replay: &world,
            verifier: &world,
            clock: &world,
        };
        // A request proof is not an introduction.
        assert_eq!(
            accept.execute(
                &Proof {
                    context: SignatureContext::Request,
                    ..answer.clone()
                },
                &introduction,
                &policy()
            ),
            Err(AuthenticationError::KeyPurposeMismatch.into())
        );
        assert_eq!(accept.execute(&answer, &introduction, &policy()), Ok(root));
        let peer = world.key("zQmPeer").unwrap().unwrap();
        assert_eq!(
            (peer.epoch.subject, peer.epoch.purpose),
            (introduced, KeyPurpose::Descriptor)
        );
        // The introduced node's key cannot sign introductions itself.
        let chained = Proof {
            context: SignatureContext::Introduction,
            key_id: "zQmPeer".to_owned(),
            subject: introduced,
            nonce: vec![9; 16],
            ..proof()
        };
        assert_eq!(
            accept.execute(&chained, &introduction, &policy()),
            Err(AuthenticationError::KeyPurposeMismatch.into())
        );
        // A workload's key introduces nothing either.
        assert_eq!(
            accept.execute(
                &Proof {
                    context: SignatureContext::Introduction,
                    nonce: vec![10; 16],
                    ..proof()
                },
                &introduction,
                &policy()
            ),
            Err(AuthenticationError::UnknownKey.into())
        );
    }
}
