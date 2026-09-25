//! ADR 0026: one PostgreSQL transaction per node action.

use aseman_capsule::{CapsuleStore, CapsuleStoreError};
use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery, CapsuleValue,
    MAX_QUERY_LIMIT, OwnerScope, StorageClass,
};
use aseman_storage_postgres::PostgresCapsuleRepository;
use aseman_storage_postgres::unit_of_work::PostgresUnitOfWorkFactory;
use postgres::{Client, Config, NoTls};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

fn creature_type(id: u8, name: &str) -> CapsuleEnvelope {
    CapsuleEnvelope {
        encoding_version: 1,
        id: CapsuleId([id; 16]),
        kind: CapsuleKind("core.creature_type".to_owned()),
        storage_class: StorageClass::Core,
        owner_scope: OwnerScope::Global,
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
        relationships: Vec::new(),
        body: Some(CapsuleValue::Object(BTreeMap::from([
            ("type_name".to_owned(), CapsuleValue::Text(name.to_owned())),
            ("document".to_owned(), CapsuleValue::Object(BTreeMap::new())),
            (
                "document_path".to_owned(),
                CapsuleValue::Text("spec".to_owned()),
            ),
            ("entry_count".to_owned(), CapsuleValue::Integer(0)),
            (
                "content_digest".to_owned(),
                CapsuleValue::Bytes(vec![1; 32]),
            ),
        ]))),
    }
    .seal()
    .unwrap()
}

fn all_types() -> CapsuleQuery {
    CapsuleQuery {
        kind: CapsuleKind("core.creature_type".to_owned()),
        predicate: None,
        projection: BTreeSet::new(),
        sort: Vec::new(),
        aggregates: Vec::new(),
        traversals: Vec::new(),
        limit: MAX_QUERY_LIMIT,
        cursor: None,
    }
}

#[test]
fn live_units_of_work_are_atomic_isolated_and_fenced() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping unit-of-work test");
        return;
    };
    let database = format!("aseman_unit_of_work_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);
    let repository = PostgresCapsuleRepository::from_client(config.connect(NoTls).unwrap());
    repository.migrate().unwrap();
    // The admin URL may carry a default database path; the pool must target the new
    // database instead, so keep only the authority and append the fresh database.
    let (scheme, rest) = admin_uri.split_once("://").unwrap();
    let authority = rest.split('/').next().unwrap();
    let uri = format!("{scheme}://{authority}/{database}");
    let factory = PostgresUnitOfWorkFactory::connect(&uri, 4, Some(5)).unwrap();
    let kind = CapsuleKind("core.creature_type".to_owned());

    // A unit reads its own writes; nobody else sees them until it commits.
    let unit = factory.begin().unwrap();
    let first = creature_type(1, "first");
    unit.put(&first, None).unwrap();
    assert_eq!(unit.get(&kind, &first.id).unwrap(), Some(first.clone()));
    assert_eq!(unit.query(&all_types()).unwrap(), vec![first.clone()]);
    assert_eq!(repository.get(&kind, &first.id).unwrap(), None);
    unit.commit().unwrap();
    assert_eq!(
        repository.get(&kind, &first.id).unwrap(),
        Some(first.clone())
    );

    // A rolled-back or dropped unit leaves nothing behind.
    let unit = factory.begin().unwrap();
    unit.put(&creature_type(2, "second"), None).unwrap();
    unit.rollback().unwrap();
    {
        let unit = factory.begin().unwrap();
        unit.put(&creature_type(3, "third"), None).unwrap();
    }
    assert_eq!(repository.query(&all_types()).unwrap(), vec![first.clone()]);

    // A refused write does not poison the transaction; the other writes commit.
    let unit = factory.begin().unwrap();
    let clash = CapsuleEnvelope {
        id: CapsuleId([4; 16]),
        ..first.clone()
    }
    .seal()
    .unwrap();
    assert_eq!(unit.put(&clash, None), Err(CapsuleStoreError::Conflict));
    let fourth = creature_type(5, "fourth");
    unit.put(&fourth, None).unwrap();
    unit.commit().unwrap();
    assert_eq!(repository.get(&kind, &fourth.id).unwrap(), Some(fourth));

    // Once the fence passes the unit's generation, its writes are refused.
    repository.raise_fence(6).unwrap();
    let unit = factory.begin().unwrap();
    assert_eq!(
        unit.put(&creature_type(6, "fenced"), None),
        Err(CapsuleStoreError::Conflict)
    );
    unit.rollback().unwrap();

    drop(factory);
    drop(repository);
    admin
        .batch_execute(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .unwrap();
}
