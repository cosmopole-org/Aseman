//! Live proof for the normalized PostgreSQL compatibility transaction.

use std::str::FromStr;

use aseman_storage_postgres::PostgresCapsuleRepository;
use aseman_storage_postgres::compatibility::PostgresCompatibilityTransactionFactory;
use postgres::{Client, Config, NoTls};

#[test]
fn compatibility_needs_are_atomic_and_relational() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping compatibility transaction test");
        return;
    };
    let database = format!("aseman_compat_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);
    let repository = PostgresCapsuleRepository::from_client(config.connect(NoTls).unwrap());
    repository.migrate().unwrap();
    let (scheme, rest) = admin_uri.split_once("://").unwrap();
    let authority = rest.split('/').next().unwrap();
    let uri = format!("{scheme}://{authority}/{database}");
    let factory = PostgresCompatibilityTransactionFactory::connect(&uri, 3).unwrap();

    let transaction = factory.begin(false).unwrap();
    transaction
        .put_object_column("Creature", "alice", "|", &[1])
        .unwrap();
    transaction
        .put_object_column("Creature", "alice", "status", b"active")
        .unwrap();
    transaction
        .put_secondary_index("Creature", "username", "id", "alice", b"alice")
        .unwrap();
    transaction
        .put_relation(
            "member",
            "store-1",
            "alice",
            "member::store-1::alice",
            "read",
        )
        .unwrap();
    transaction
        .put_document(
            "Json::Creature::alice",
            "metadata",
            &serde_json::json!({"display": "Alice"}),
        )
        .unwrap();
    transaction
        .put_opaque("counter", &7_i64.to_be_bytes())
        .unwrap();
    assert_eq!(
        transaction.value("obj::Creature::alice::status").unwrap(),
        Some(b"active".to_vec())
    );
    assert_eq!(
        transaction
            .keys_with_prefix("link::member::store-1::")
            .unwrap(),
        ["link::member::store-1::alice"]
    );
    transaction.commit().unwrap();

    let transaction = factory.begin(false).unwrap();
    assert_eq!(
        transaction
            .document("Json::Creature::alice", "metadata")
            .unwrap(),
        Some(serde_json::json!({"display": "Alice"}))
    );
    transaction
        .delete_key("link::member::store-1::alice")
        .unwrap();
    transaction.rollback().unwrap();

    let mut direct = Config::from_str(&admin_uri).unwrap();
    direct.dbname(&database);
    let mut direct = direct.connect(NoTls).unwrap();
    let row = direct
        .query_one(
            "SELECT relation_type, scope, member, value FROM aseman_compat.relations",
            &[],
        )
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "member");
    assert_eq!(row.get::<_, String>(1), "store-1");
    assert_eq!(row.get::<_, String>(2), "alice");
    assert_eq!(row.get::<_, String>(3), "read");

    drop(direct);
    drop(factory);
    drop(repository);
    admin
        .batch_execute(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .unwrap();
}
