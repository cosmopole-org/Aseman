//! A401/A403 identity storage on live PostgreSQL: the capsule key directory, grant
//! store, replay guard, and challenge store pass their port conformance suites, and the
//! retirement migration removed `node_keys`.

use aseman_capsule::identity::CapsuleKeyDirectory;
use aseman_storage_postgres::PostgresCapsuleRepository;
use postgres::{Client, Config, NoTls};
use std::str::FromStr;

#[test]
fn live_identity_storage_passes_conformance() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping identity storage test");
        return;
    };
    let database = format!("aseman_identity_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);
    let repository = PostgresCapsuleRepository::from_client(config.connect(NoTls).unwrap());
    repository.migrate().unwrap();
    // Idempotent: every start runs every migration.
    repository.migrate().unwrap();

    aseman_ports::conformance::key_directory(&CapsuleKeyDirectory {
        repository: &repository,
    });
    aseman_ports::conformance::grant_store(&aseman_capsule::capability::CapsuleGrantStore {
        repository: &repository,
    });
    aseman_ports::conformance::decision_audit(&aseman_capsule::audit::CapsuleDecisionAudit {
        repository: &repository,
    });
    aseman_ports::conformance::replay_guard(&repository);
    aseman_ports::conformance::challenge_store(&repository);
    // Two nonces retained until 2000 and the unused challenge expiring at 2000.
    assert_eq!(repository.purge_expired_nonces(3_000), Ok(3));

    let mut client = config.connect(NoTls).unwrap();
    let node_keys: Option<String> = client
        .query_one("SELECT to_regclass('aseman_core.node_keys')::text", &[])
        .unwrap()
        .get(0);
    assert_eq!(node_keys, None);

    drop(client);
    drop(repository);
    admin
        .batch_execute(&format!("DROP DATABASE {database}"))
        .unwrap();
}

#[test]
fn live_migration_retires_the_writerless_shapes() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping retirement test");
        return;
    };
    let database = format!("aseman_retire_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);
    let mut client = config.connect(NoTls).unwrap();
    // The shapes an earlier release created.
    client
        .batch_execute(
            "CREATE SCHEMA aseman_core;
             CREATE TABLE aseman_core.node_keys (id UUID PRIMARY KEY);
             CREATE TABLE aseman_core.capability_grants (id UUID PRIMARY KEY, action TEXT);",
        )
        .unwrap();
    let repository = PostgresCapsuleRepository::from_client(config.connect(NoTls).unwrap());
    repository.migrate().unwrap();
    let column = |table: &str, column: &str| -> bool {
        config
            .connect(NoTls)
            .unwrap()
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
                 WHERE table_schema = 'aseman_core' AND table_name = $1 AND column_name = $2)",
                &[&table, &column],
            )
            .unwrap()
            .get(0)
    };
    assert!(!column("capability_grants", "action"));
    assert!(column("capability_grants", "max_depth"));
    assert!(column("identity_keys", "key_id"));
    let node_keys: Option<String> = client
        .query_one("SELECT to_regclass('aseman_core.node_keys')::text", &[])
        .unwrap()
        .get(0);
    assert_eq!(node_keys, None);
    // A second start leaves the current shapes alone.
    repository.migrate().unwrap();
    assert!(column("capability_grants", "max_depth"));

    drop(client);
    drop(repository);
    admin
        .batch_execute(&format!("DROP DATABASE {database}"))
        .unwrap();
}
