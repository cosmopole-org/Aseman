//! The creature ports run on capsules with the rules the legacy adapter
//! characterizes (RL-004 strangler, target side).

use aseman_capsule_repositories::CapsuleStore;
use aseman_capsule_repositories::creature::CapsuleCreaturePorts;
use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleValue};
use aseman_contracts::legacy_realtime::deterministic_legacy_capsule_id;
use aseman_domain::creature::CreatureRecord;
use aseman_ports::{CreatureBalances, CreatureDirectory, PortError};
use aseman_storage_postgres::PostgresCapsuleRepository;
use postgres::{Client, Config, NoTls};
use rsa::pkcs8::{EncodePublicKey, LineEnding};
use std::collections::BTreeMap;
use std::str::FromStr;

fn public_keys() -> [String; 5] {
    [0, 1, 2, 3, 4].map(|_| {
        rsa::RsaPublicKey::from(&rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap())
            .to_public_key_pem(LineEnding::LF)
            .unwrap()
    })
}

#[test]
fn live_creature_ports_pass_conformance_on_capsules() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping creature ports test");
        return;
    };
    let database = format!("aseman_creature_ports_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);
    let repository = PostgresCapsuleRepository::from_client(config.connect(NoTls).unwrap());
    repository.migrate().unwrap();
    let ports = CapsuleCreaturePorts {
        repository: &repository,
        currency: "USD",
        scale: 2,
    };
    let keys = public_keys();
    aseman_ports::conformance::creature_directory(&ports, &ports, [&keys[0], &keys[1], &keys[2]]);
    aseman_ports::conformance::creature_metadata(&ports, &ports, &keys[4]);
    aseman_ports::conformance::creature_types(&ports);

    // Humans without an email coexist: an absent email is NULL, not a unique "".
    let carol = CreatureRecord {
        id: "7@conformance".to_owned(),
        creature_type: "human".to_owned(),
        username: "carol@conformance".to_owned(),
        // Another spelling of a key normalizes to the standard PEM.
        public_key: keys[3].replace('\n', "\r\n"),
        chain_id: "main".to_owned(),
        subchain_id: "main".to_owned(),
        owner_id: "free".to_owned(),
    };
    ports.create(&carol).unwrap();
    ports.open(&carol.id, 3).unwrap();
    let stored = ports.creature(&carol.id).unwrap().unwrap();
    assert_eq!(stored.public_key, keys[3]);
    assert_eq!(ports.balance(&carol.id), Ok(3));

    // A machine must be owned by a human user, as the A308 export requires.
    let orphan = CreatureRecord {
        id: "8@conformance".to_owned(),
        creature_type: "machine".to_owned(),
        username: "orphan@conformance".to_owned(),
        public_key: keys[3].clone(),
        chain_id: "main".to_owned(),
        subchain_id: "main".to_owned(),
        owner_id: "9@nowhere".to_owned(),
    };
    assert!(matches!(ports.create(&orphan), Err(PortError::Failed(_))));

    // `put_all` is atomic: when a later write loses on a unique index, the earlier
    // writes of the same transaction are rolled back.
    let identity_kind = CapsuleKind("core.legacy_identity".to_owned());
    let carol_identity = repository
        .get(
            &identity_kind,
            &CapsuleId(deterministic_legacy_capsule_id(
                "LegacyIdentity",
                b"Creature\x007@conformance",
            )),
        )
        .unwrap()
        .unwrap();
    let fresh_identity = CapsuleEnvelope {
        id: CapsuleId(deterministic_legacy_capsule_id(
            "LegacyIdentity",
            b"Creature\x009@conformance",
        )),
        body: Some(CapsuleValue::Object(BTreeMap::from([
            (
                "family".to_owned(),
                CapsuleValue::Text("Creature".to_owned()),
            ),
            (
                "legacy_id".to_owned(),
                CapsuleValue::Text("9@conformance".to_owned()),
            ),
            (
                "target_kind".to_owned(),
                CapsuleValue::Text("core.creature".to_owned()),
            ),
            ("target_id".to_owned(), CapsuleValue::Bytes(vec![9; 16])),
        ]))),
        ..carol_identity
    }
    .seal()
    .unwrap();
    let carol_user = repository
        .get(
            &CapsuleKind("core.user".to_owned()),
            &CapsuleId(deterministic_legacy_capsule_id("User", b"7@conformance")),
        )
        .unwrap()
        .unwrap();
    let username_clash = CapsuleEnvelope {
        id: CapsuleId(deterministic_legacy_capsule_id("User", b"9@conformance")),
        ..carol_user
    }
    .seal()
    .unwrap();
    assert_eq!(
        CapsuleStore::put_all(
            &repository,
            &[(fresh_identity.clone(), None), (username_clash, None)]
        ),
        Err(aseman_capsule_repositories::CapsuleStoreError::Conflict)
    );
    assert_eq!(
        repository.get(&identity_kind, &fresh_identity.id).unwrap(),
        None
    );

    drop(repository);
    admin
        .batch_execute(&format!("DROP DATABASE {database}"))
        .unwrap();
}
