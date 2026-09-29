//! ADR 0016: document capsules persist through a real PostgreSQL schema without a
//! native document column, keep their subject foreign key and uniqueness, and refuse
//! filtering inside the document.
use aseman_config::CapsuleLayout;
use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery, CapsuleRelationship,
    CapsuleValue, ComparisonOperator, OwnerScope, QueryPredicate, StorageClass,
};
use aseman_storage_postgres::{PostgresCapsuleRepository, PostgresStorageError};
use postgres::{Client, NoTls};
use std::collections::{BTreeMap, BTreeSet};

/// Both live tests truncate the shared `aseman_core` tables, so they run one at a time.
static LIVE_DATABASE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn capsule(
    id: u8,
    kind: &str,
    owner_scope: OwnerScope,
    relationships: Vec<(&str, &str, u8)>,
    body: Vec<(&str, CapsuleValue)>,
) -> CapsuleEnvelope {
    CapsuleEnvelope {
        encoding_version: 1,
        id: CapsuleId([id; 16]),
        kind: CapsuleKind(kind.to_owned()),
        storage_class: StorageClass::Core,
        owner_scope,
        schema_version: 1,
        revision: 1,
        created_at_micros: 100,
        updated_at_micros: 100,
        previous_integrity: None,
        integrity_hash: CapsuleDigest {
            algorithm: "sha2-256".to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: false,
        relationships: relationships
            .into_iter()
            .map(|(name, target_kind, target)| CapsuleRelationship {
                name: name.to_owned(),
                target_kind: CapsuleKind(target_kind.to_owned()),
                target_id: CapsuleId([target; 16]),
            })
            .collect(),
        body: Some(CapsuleValue::Object(
            body.into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
        )),
    }
    .seal()
    .unwrap()
}

fn text(value: &str) -> CapsuleValue {
    CapsuleValue::Text(value.to_owned())
}

fn program_metadata(id: u8, document: CapsuleValue) -> CapsuleEnvelope {
    capsule(
        id,
        "core.program_metadata",
        OwnerScope::Creature([2; 16]),
        vec![("program", "core.program", 3)],
        vec![
            ("document", document),
            ("document_path", text("metadata")),
            ("entry_count", CapsuleValue::Integer(1)),
            ("content_digest", CapsuleValue::Bytes(vec![7; 32])),
        ],
    )
}

#[test]
fn live_postgres_round_trips_document_capsules_in_both_layouts() {
    let _serial = LIVE_DATABASE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(connection_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url
    else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping disposable PostgreSQL test");
        return;
    };
    let repository = PostgresCapsuleRepository::connect(&connection_uri).unwrap();
    repository.migrate_layout(CapsuleLayout::Flattened).unwrap();
    repository.migrate().unwrap();
    assert_eq!(repository.layout(), CapsuleLayout::Flattened);
    Client::connect(&connection_uri, NoTls)
        .unwrap()
        .batch_execute("TRUNCATE TABLE aseman_core.users CASCADE")
        .unwrap();

    let user = capsule(
        1,
        "core.user",
        OwnerScope::Global,
        Vec::new(),
        vec![
            ("username", text("doc-owner")),
            ("public_key", CapsuleValue::Bytes(vec![1; 8])),
            ("status", text("active")),
        ],
    );
    let creature = capsule(
        2,
        "core.creature",
        OwnerScope::Global,
        vec![("owner", "core.user", 1)],
        vec![
            ("username", text("doc-machine")),
            ("creature_type", text("machine")),
            ("public_key", CapsuleValue::Bytes(vec![2; 8])),
            ("status", text("active")),
        ],
    );
    let program = capsule(
        3,
        "core.program",
        OwnerScope::Creature([2; 16]),
        vec![("creature", "core.creature", 2)],
        vec![
            ("machine_id", text("doc-machine")),
            ("runtime", text("wasm")),
            ("path", text("/doc")),
        ],
    );
    for capsule in [&user, &creature, &program] {
        repository.put(capsule, None).unwrap();
    }

    let document = CapsuleValue::Object(BTreeMap::from([(
        "manifest".to_owned(),
        CapsuleValue::Object(BTreeMap::from([
            (
                "tools".to_owned(),
                CapsuleValue::Array(vec![text("a"), CapsuleValue::Null]),
            ),
            ("ratio".to_owned(), CapsuleValue::Float(0.25)),
        ])),
    )]));
    let metadata = program_metadata(4, document);
    repository.put(&metadata, None).unwrap();
    assert_eq!(
        repository.get(&metadata.kind, &metadata.id).unwrap(),
        Some(metadata.clone())
    );

    let mut client = Client::connect(&connection_uri, NoTls).unwrap();
    let columns = client
        .query(
            "SELECT column_name FROM information_schema.columns \
             WHERE table_schema = 'aseman_core' AND table_name = 'program_metadata_documents'",
            &[],
        )
        .unwrap()
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    assert!(columns.contains("document") && columns.contains("capsule_shape"));
    assert!(columns.contains("content_digest") && columns.contains("program"));

    // Flattened (the default): every field is a real column, nothing is packed.
    let id = uuid::Uuid::from_bytes(metadata.id.0);
    let stored = |client: &mut Client| {
        let row = client
            .query_one(
                "SELECT capsule_cbor IS NULL, document, document_path                  FROM aseman_core.program_metadata_documents WHERE id = $1",
                &[&id],
            )
            .unwrap();
        (
            row.get::<_, bool>(0),
            row.get::<_, Option<serde_json::Value>>(1),
            row.get::<_, Option<String>>(2),
        )
    };
    let manifest = serde_json::json!({
        "manifest": {"tools": ["a", null], "ratio": {"$float": "0.25"}}
    });
    assert_eq!(
        stored(&mut client),
        (true, Some(manifest.clone()), Some("metadata".to_owned()))
    );

    // Capsule mode packs every mutable row; switching back flattens it again. Reads
    // return the exact capsule throughout.
    repository.migrate_layout(CapsuleLayout::Capsule).unwrap();
    assert_eq!(
        stored(&mut client),
        (false, None, Some("metadata".to_owned()))
    );
    assert_eq!(
        PostgresCapsuleRepository::connect(&connection_uri)
            .unwrap()
            .layout(),
        CapsuleLayout::Capsule
    );
    assert_eq!(
        repository.get(&metadata.kind, &metadata.id).unwrap(),
        Some(metadata.clone())
    );
    repository.migrate_layout(CapsuleLayout::Flattened).unwrap();
    assert_eq!(
        stored(&mut client),
        (true, Some(manifest), Some("metadata".to_owned()))
    );
    assert_eq!(
        repository.get(&metadata.kind, &metadata.id).unwrap(),
        Some(metadata.clone())
    );

    // A flattened row edited outside the provider no longer matches its integrity hash.
    client
        .execute(
            "UPDATE aseman_core.program_metadata_documents SET document_path = 'edited' WHERE id = $1",
            &[&id],
        )
        .unwrap();
    assert!(matches!(
        repository.get(&metadata.kind, &metadata.id),
        Err(PostgresStorageError::Invalid(_))
    ));
    client
        .execute(
            "UPDATE aseman_core.program_metadata_documents SET document_path = 'metadata' WHERE id = $1",
            &[&id],
        )
        .unwrap();

    // One document per subject; the subject foreign key is enforced natively.
    assert!(matches!(
        repository.put(
            &program_metadata(5, CapsuleValue::Object(BTreeMap::new())),
            None
        ),
        Err(PostgresStorageError::Conflict)
    ));
    let dangling = capsule(
        6,
        "core.program_metadata",
        OwnerScope::Creature([2; 16]),
        vec![("program", "core.program", 9)],
        vec![
            ("document", CapsuleValue::Object(BTreeMap::new())),
            ("document_path", text("metadata")),
            ("entry_count", CapsuleValue::Integer(0)),
            ("content_digest", CapsuleValue::Bytes(vec![7; 32])),
        ],
    );
    assert!(matches!(
        repository.put(&dangling, None),
        Err(PostgresStorageError::Invalid(_))
    ));

    let query = |predicate| CapsuleQuery {
        kind: CapsuleKind("core.program_metadata".to_owned()),
        predicate: Some(predicate),
        projection: BTreeSet::new(),
        sort: Vec::new(),
        aggregates: Vec::new(),
        traversals: Vec::new(),
        limit: 10,
        cursor: None,
    };
    assert!(
        repository
            .query(&query(QueryPredicate::Compare {
                field: "document".to_owned(),
                operator: ComparisonOperator::Equal,
                value: text("x"),
            }))
            .is_err()
    );
    assert_eq!(
        repository
            .query(&query(QueryPredicate::Compare {
                field: "document_path".to_owned(),
                operator: ComparisonOperator::Equal,
                value: text("metadata"),
            }))
            .unwrap(),
        vec![metadata]
    );
}

#[test]
fn live_postgres_fences_old_generations() {
    let _serial = LIVE_DATABASE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(connection_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url
    else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping disposable PostgreSQL test");
        return;
    };
    let repository = PostgresCapsuleRepository::connect(&connection_uri).unwrap();
    repository.migrate().unwrap();
    let mut client = Client::connect(&connection_uri, NoTls).unwrap();
    client
        .batch_execute(
            "DO $$ DECLARE r record; BEGIN \
               FOR r IN SELECT schemaname, tablename FROM pg_tables \
                 WHERE schemaname IN ('aseman_core', 'aseman_telemetry', 'aseman_audit', \
                   'aseman_finance', 'aseman_outbox', 'aseman_realtime') \
                   AND tablename <> 'migration_fence' \
               LOOP EXECUTE format('TRUNCATE TABLE %I.%I CASCADE', r.schemaname, r.tablename); \
               END LOOP; END $$; \
             UPDATE aseman_core.migration_fence SET min_generation = 0",
        )
        .unwrap();
    let user = capsule(
        31,
        "core.user",
        OwnerScope::Global,
        Vec::new(),
        vec![
            ("username", text("fenced-user")),
            ("public_key", CapsuleValue::Bytes(vec![31; 8])),
            ("status", text("active")),
        ],
    );
    let write = |generation| repository.put_fenced(&user, None, Some(generation));
    repository.raise_fence(7).unwrap();
    // A fence only rises.
    repository.raise_fence(3).unwrap();
    assert!(matches!(write(6), Err(PostgresStorageError::Conflict)));
    write(7).unwrap();
    // An identical replay is idempotent.
    write(7).unwrap();
}
