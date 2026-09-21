//! The store use cases run through the capsule adapter with the same rules the legacy
//! adapter characterizes (RL-004 strangler, target side).

use aseman_application::store::{GetStoreAccess, ReadStoreHistory, SetStoreAccess, SignalStore};
use aseman_capsule_repositories::store::CapsuleStorePorts;
use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleRelationship, CapsuleValue,
    OwnerScope, StorageClass,
};
use aseman_contracts::legacy_realtime::{SignalStreamPolicy, deterministic_legacy_capsule_id};
use aseman_domain::signal_tags::LogQuery;
use aseman_domain::store_permissions::StorePermissions;
use aseman_ports::{ClockPort, StoreAccess, StoreDirectory};
use aseman_storage_postgres::PostgresCapsuleRepository;
use postgres::{Client, Config, NoTls};
use std::collections::BTreeMap;
use std::str::FromStr;

fn sealed(
    family: &str,
    legacy: &str,
    kind: &str,
    owner: OwnerScope,
    relationships: Vec<(&str, &str, &str, &str)>,
    body: Vec<(&str, CapsuleValue)>,
) -> CapsuleEnvelope {
    CapsuleEnvelope {
        encoding_version: 1,
        id: CapsuleId(deterministic_legacy_capsule_id(family, legacy.as_bytes())),
        kind: CapsuleKind(kind.to_owned()),
        storage_class: StorageClass::Core,
        owner_scope: owner,
        schema_version: 1,
        revision: 1,
        created_at_micros: 1,
        updated_at_micros: 1,
        previous_integrity: None,
        integrity_hash: CapsuleDigest {
            algorithm: "sha2-256".to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: false,
        relationships: relationships
            .into_iter()
            .map(
                |(name, target_kind, target_family, target)| CapsuleRelationship {
                    name: name.to_owned(),
                    target_kind: CapsuleKind(target_kind.to_owned()),
                    target_id: CapsuleId(deterministic_legacy_capsule_id(
                        target_family,
                        target.as_bytes(),
                    )),
                },
            )
            .collect(),
        body: Some(CapsuleValue::Object(
            body.into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect::<BTreeMap<_, _>>(),
        )),
    }
    .seal()
    .unwrap()
}

fn text(value: &str) -> CapsuleValue {
    CapsuleValue::Text(value.to_owned())
}

struct Clock;
impl ClockPort for Clock {
    fn unix_millis(&self) -> i64 {
        1_700_000_000_000
    }
}

#[test]
fn live_store_use_cases_run_on_capsules_with_legacy_rules() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping store ports test");
        return;
    };
    let database = format!("aseman_store_ports_{}", uuid::Uuid::now_v7().simple());
    Client::connect(&admin_uri, NoTls)
        .unwrap()
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);
    let repository = PostgresCapsuleRepository::from_client(config.connect(NoTls).unwrap());
    repository.migrate().unwrap();

    let creature_scope =
        OwnerScope::Creature(deterministic_legacy_capsule_id("Creature", b"1@global"));
    for capsule in [
        sealed(
            "User",
            "1@global",
            "core.user",
            OwnerScope::Global,
            vec![],
            vec![
                ("username", text("alice")),
                ("public_key", CapsuleValue::Bytes(vec![1; 8])),
                ("status", text("active")),
            ],
        ),
        sealed(
            "Creature",
            "1@global",
            "core.creature",
            OwnerScope::Global,
            vec![("owner", "core.user", "User", "1@global")],
            vec![
                ("username", text("alice")),
                ("creature_type", text("human")),
                ("public_key", CapsuleValue::Bytes(vec![2; 8])),
                ("status", text("active")),
            ],
        ),
        sealed(
            "Store",
            "3@global",
            "core.store",
            creature_scope.clone(),
            vec![("creature", "core.creature", "Creature", "1@global")],
            vec![
                ("is_public", CapsuleValue::Bool(false)),
                ("member_count", CapsuleValue::Integer(1)),
                ("persistent_history", CapsuleValue::Bool(true)),
                ("signal_count", CapsuleValue::Integer(0)),
                ("tag", text("events")),
            ],
        ),
    ] {
        repository.put(&capsule, None).unwrap();
    }
    let policy = |_: &str| SignalStreamPolicy {
        authorization_scope: vec![1],
        retention_class: "persistent".to_owned(),
    };
    let ports = CapsuleStorePorts {
        repository: &repository,
        stream_policy: &policy,
    };
    ports
        .set_permissions("3@global", "1@global", StorePermissions::owner())
        .unwrap();

    let signal = SignalStore {
        stores: &ports,
        access: &ports,
        log: &ports,
        clock: &Clock,
    };
    for (data, tags) in [
        ("one", vec!["t=a".to_owned()]),
        ("two", vec![]),
        ("three", vec!["t=a".to_owned()]),
    ] {
        assert!(
            signal
                .execute("1@global", "3@global", data, &tags, false)
                .unwrap()
                .persisted
        );
    }
    assert!(
        !signal
            .execute("1@global", "3@global", "typing", &[], true)
            .unwrap()
            .persisted
    );
    assert_eq!(ports.store("3@global").unwrap().unwrap().signal_count, 3);

    let history = ReadStoreHistory {
        access: &ports,
        log: &ports,
    };
    let all = history
        .execute("1@global", "3@global", LogQuery::default())
        .unwrap();
    assert_eq!(
        all.iter()
            .map(|signal| signal.data.as_str())
            .collect::<Vec<_>>(),
        vec!["three", "two", "one"]
    );
    let tagged = history
        .execute(
            "1@global",
            "3@global",
            LogQuery {
                tags_all: vec!["t=a".to_owned()],
                count: 1,
                ..LogQuery::default()
            },
        )
        .unwrap();
    assert_eq!(tagged.len(), 1);
    assert_eq!(tagged[0].data, "three");

    // A remote principal gains read access; the stored permission text is canonical.
    let set = SetStoreAccess { access: &ports };
    assert_eq!(
        set.execute("1@global", "3@global", "9@remote", &["read".to_owned()])
            .unwrap(),
        StorePermissions::viewer()
    );
    let get = GetStoreAccess { access: &ports };
    assert_eq!(
        get.execute("9@remote", "3@global", "").unwrap().1,
        StorePermissions::viewer()
    );
    assert!(
        signal
            .execute("9@remote", "3@global", "x", &[], false)
            .is_err()
    );
    assert_eq!(
        history
            .execute("9@remote", "3@global", LogQuery::default())
            .unwrap()
            .len(),
        3
    );
    // Upgrading an existing grant writes a new chained revision.
    set.execute(
        "1@global",
        "3@global",
        "9@remote",
        &["read".to_owned(), "signal".to_owned()],
    )
    .unwrap();
    assert_eq!(
        get.execute("1@global", "3@global", "9@remote").unwrap().1,
        StorePermissions::member()
    );

    drop(repository);
    Client::connect(&admin_uri, NoTls)
        .unwrap()
        .batch_execute(&format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)"))
        .unwrap();
}
