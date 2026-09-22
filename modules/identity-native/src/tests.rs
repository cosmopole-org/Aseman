//! End to end over the committed A401 vectors: the contract proof authenticates through
//! the application use case with this verifier, once.

use super::*;
use aseman_application::identity::{AuthenticateProof, IdentityFailure, VerifierPolicy};
use aseman_contracts::identity::SignedRequestProof;
use aseman_domain::identity::{FreshnessPolicy, KeyEpoch, KeyPurpose, RotationPolicy, Subject};
use aseman_ports::{ClockPort, KeyDirectory, PortResult, ReplayGuard};
use std::collections::BTreeSet;
use std::sync::Mutex;

const VECTORS: &str = include_str!("../../../contracts/security/vectors/identity-v1.json");

struct Fixture {
    key: IdentityKey,
    nonces: Mutex<BTreeSet<Vec<u8>>>,
    now: i64,
}

impl KeyDirectory for Fixture {
    fn key(&self, key_id: &str) -> PortResult<Option<IdentityKey>> {
        Ok((key_id == self.key.key_id).then(|| self.key.clone()))
    }
    fn epochs(&self, subject: &Subject, purpose: KeyPurpose) -> PortResult<Vec<IdentityKey>> {
        Ok(
            (self.key.epoch.subject == *subject && self.key.epoch.purpose == purpose)
                .then(|| self.key.clone())
                .into_iter()
                .collect(),
        )
    }
    fn register(&self, _: &IdentityKey) -> PortResult<()> {
        unreachable!()
    }
    fn retire(&self, _: &str, _: i64) -> PortResult<()> {
        unreachable!()
    }
    fn revoke(&self, _: &str, _: i64) -> PortResult<()> {
        unreachable!()
    }
}

impl ReplayGuard for Fixture {
    fn record_nonce(&self, _: &str, nonce: &[u8], _: i64, _: i64) -> PortResult<bool> {
        Ok(self.nonces.lock().unwrap().insert(nonce.to_vec()))
    }
}

impl ClockPort for Fixture {
    fn unix_millis(&self) -> i64 {
        self.now
    }
}

#[test]
fn the_contract_vector_authenticates_once_through_the_use_case() {
    let vectors: serde_json::Value = serde_json::from_str(VECTORS).unwrap();
    let wire: SignedRequestProof =
        serde_json::from_value(vectors["request"]["proof"].clone()).unwrap();
    let proof = wire.parse().unwrap();
    let public_key =
        PublicKey::from_multibase(vectors["key"]["multibase"].as_str().unwrap()).unwrap();
    let fixture = Fixture {
        key: IdentityKey {
            key_id: public_key.key_id(),
            public_key: public_key.encode(),
            epoch: KeyEpoch {
                subject: proof.subject,
                purpose: KeyPurpose::Authentication,
                epoch: proof.key_epoch,
                not_before_millis: 0,
                expires_at_millis: None,
                retired_at_millis: None,
                revoked_at_millis: None,
                legacy: false,
            },
        },
        nonces: Mutex::new(BTreeSet::new()),
        now: proof.window.issued_at_millis + 1_000,
    };
    let authenticate = AuthenticateProof {
        keys: &fixture,
        replay: &fixture,
        verifier: &NativeIdentityVerifier,
        clock: &fixture,
    };
    let policy = VerifierPolicy {
        audience: proof.audience.clone(),
        freshness: FreshnessPolicy::GUEST,
        rotation: RotationPolicy::DEFAULT,
    };
    let body = vectors["request"]["body_utf8"].as_str().unwrap().as_bytes();
    assert_eq!(
        authenticate.execute(&proof, body, &policy),
        Ok(proof.subject)
    );
    assert_eq!(
        authenticate.execute(&proof, body, &policy),
        Err(IdentityFailure::Rejected(AuthenticationError::Replayed))
    );
    assert_eq!(
        authenticate.execute(&proof, b"another body", &policy),
        Err(IdentityFailure::Rejected(
            AuthenticationError::BodyDigestMismatch
        ))
    );
    // A directory whose key ID does not name the stored key is refused.
    let mut wrong = fixture.key.clone();
    wrong.key_id = "zQmNotThisKey".to_owned();
    assert_eq!(
        NativeIdentityVerifier.verify(&proof, &wrong),
        Err(AuthenticationError::Malformed)
    );
}
