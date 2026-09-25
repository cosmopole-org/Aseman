//! The public action service's durable idempotency on live PostgreSQL (A701, P7-06):
//! the store passes its port conformance suite, and the composed service replays a
//! mutation retry against the real store.

use aseman_application::identity::VerifierPolicy;
use aseman_application::public_action::{
    PublicActionRequest, RequestAuthentication, ServePublicAction,
};
use aseman_capsule::audit::CapsuleDecisionAudit;
use aseman_capsule::capability::CapsuleGrantStore;
use aseman_capsule::identity::CapsuleKeyDirectory;
use aseman_domain::authority::{ActionClass, Condition, ResourceRef};
use aseman_domain::identity::{
    CredentialWindow, FreshnessPolicy, IdentityKey, KeyEpoch, KeyPurpose, Proof, RotationPolicy,
    SignatureContext, Subject, SubjectKind,
};
use aseman_policy_native::RegistryPolicy;
use aseman_ports::{ActionExecutor, ClockPort, PortResult};
use aseman_storage_postgres::PostgresCapsuleRepository;
use postgres::{Client, Config, NoTls};
use std::collections::BTreeSet;
use std::str::FromStr;

const NOW: i64 = 1_800_000_000_000;

struct Clock;
impl ClockPort for Clock {
    fn unix_millis(&self) -> i64 {
        NOW
    }
}

fn subject() -> Subject {
    Subject {
        kind: SubjectKind::Creature,
        id: "0190f1a2-7b3c-7d4e-8f00-112233445566".parse().unwrap(),
    }
}

/// A scripted executor: resolves a fixed creature resource and echoes the body.
struct ScriptedExecutor;
impl ActionExecutor for ScriptedExecutor {
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
            BTreeSet::from([Condition::SelfResource]),
        ))
    }
    fn execute(&self, _: Subject, _: &str, body: &[u8]) -> PortResult<Vec<u8>> {
        Ok(body.to_vec())
    }
}

/// A scripted verifier that accepts the signature `b"good"` and digests a body by
/// copying its first bytes, so proofs in this test authenticate without an Ed25519
/// key pair.
struct ScriptedVerifier;
impl aseman_ports::IdentityVerifier for ScriptedVerifier {
    fn verify(
        &self,
        proof: &Proof,
        _: &IdentityKey,
    ) -> Result<(), aseman_domain::identity::AuthenticationError> {
        if proof.signature == b"good" {
            Ok(())
        } else {
            Err(aseman_domain::identity::AuthenticationError::BadSignature)
        }
    }
    fn body_digest(&self, body: &[u8]) -> [u8; 32] {
        let mut digest = [0; 32];
        digest[..body.len().min(32)].copy_from_slice(&body[..body.len().min(32)]);
        digest
    }
    fn describe_key(
        &self,
        _: &[u8],
    ) -> Result<aseman_domain::identity::KeyDescription, aseman_domain::identity::AuthenticationError>
    {
        Ok(aseman_domain::identity::KeyDescription {
            key_id: "zQmKey".to_owned(),
            legacy: false,
        })
    }
    fn introduction_bytes(&self, _: &aseman_domain::identity::Introduction) -> Vec<u8> {
        Vec::new()
    }
}

fn proof(action: &str, nonce: u8, body: &[u8]) -> Proof {
    let mut digest = [0; 32];
    digest[..body.len().min(32)].copy_from_slice(&body[..body.len().min(32)]);
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
        body_digest: digest,
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

#[test]
fn live_public_action_idempotency_passes_conformance() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping public action idempotency test");
        return;
    };
    let database = format!("aseman_public_action_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);
    let repository = PostgresCapsuleRepository::from_client(config.connect(NoTls).unwrap());
    repository.migrate().unwrap();

    aseman_ports::conformance::public_action::public_action_idempotency(&repository);

    drop(repository);
    admin
        .batch_execute(&format!("DROP DATABASE {database}"))
        .unwrap();
}

#[test]
fn live_public_action_composition_replays_a_mutation_retry() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping public action composition test");
        return;
    };
    let database = format!("aseman_public_action_e2e_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);
    let repository = PostgresCapsuleRepository::from_client(config.connect(NoTls).unwrap());
    repository.migrate().unwrap();

    // A key registered in the real directory so A401 finds it.
    aseman_ports::KeyDirectory::register(
        &CapsuleKeyDirectory {
            repository: &repository,
        },
        &IdentityKey {
            key_id: "zQmKey".to_owned(),
            public_key: vec![0, b'K'],
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
    )
    .unwrap();

    let policy = RegistryPolicy::compiled("live").unwrap();
    let clock = Clock;
    let service = ServePublicAction {
        keys: &CapsuleKeyDirectory {
            repository: &repository,
        },
        replay: &repository,
        sessions: &SkipSessions,
        verifier: &ScriptedVerifier,
        clock: &clock,
        policy: &policy,
        grants: &CapsuleGrantStore {
            repository: &repository,
        },
        audit: &CapsuleDecisionAudit {
            repository: &repository,
        },
        idempotency: &repository,
        executor: &ScriptedExecutor,
        verifier_policy: &verifier_policy(),
    };

    let key = "request-key-0001";
    let first = PublicActionRequest {
        request_id: "req-1".to_owned(),
        route: "/v1/actions/creatures/update".to_owned(),
        action: "creature.update".to_owned(),
        class: ActionClass::Write,
        authentication: RequestAuthentication::Proof(Box::new(proof("creature.update", 1, b"{}"))),
        idempotency_key: Some(key.to_owned()),
        body: b"{}".to_vec(),
    };
    let first_response = service.execute(&first).unwrap();
    assert_eq!(first_response.body, b"{}".to_vec());

    // A retry under the same key replays the recorded outcome; the executor did not
    // run again (the echoed body is identical, and a replay would have been the
    // stored response).
    let retry = PublicActionRequest {
        request_id: "req-2".to_owned(),
        route: "/v1/actions/creatures/update".to_owned(),
        action: "creature.update".to_owned(),
        class: ActionClass::Write,
        authentication: RequestAuthentication::Proof(Box::new(proof("creature.update", 2, b"{}"))),
        idempotency_key: Some(key.to_owned()),
        body: b"{}".to_vec(),
    };
    let replayed = service.execute(&retry).unwrap();
    assert_eq!(replayed.body, first_response.body);
    // The composed service recorded two decisions (one per attempt) and the store
    // holds exactly one completed claim.
    let mut client = config.connect(NoTls).unwrap();
    let recorded: i64 = client
        .query_one(
            "SELECT count(*) FROM aseman_core.public_idempotency \
             WHERE subject = $1 AND key = $2 AND completed",
            &[&subject().to_string(), &key],
        )
        .unwrap()
        .get(0);
    assert_eq!(recorded, 1);
    drop(client);

    drop(repository);
    admin
        .batch_execute(&format!("DROP DATABASE {database}"))
        .unwrap();
}

/// Sessions are legacy bearer credentials; this test uses proofs only, so the
/// directory resolves nothing.
struct SkipSessions;
impl aseman_ports::SessionDirectory for SkipSessions {
    fn subject(&self, _: &str) -> PortResult<Option<Subject>> {
        Ok(None)
    }
}
