use aseman_ports::conformance::vmm::vmm_stores;

use super::*;

/// The VMM stores against a live PostgreSQL (`ASEMAN_TEST_POSTGRES_URL`), in a fresh
/// database so the suite starts empty.
#[test]
fn live_vmm_stores_pass_the_suite() {
    let Some(url) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        return;
    };
    let database = format!("aseman_vmm_{}", Uuid::now_v7().simple());
    let mut admin = postgres::Client::connect(&url, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config: postgres::Config = url.parse().unwrap();
    config.dbname(&database);
    let store = {
        let pool = Pool::builder()
            .max_size(4)
            .build(PostgresConnectionManager::new(config, NoTls))
            .unwrap();
        PostgresVmmStore { pool }
    };
    store.migrate().unwrap();
    // Idempotent.
    store.migrate().unwrap();
    vmm_stores(&store, &store, &store, &store);
    drop(store);
    admin
        .batch_execute(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .unwrap();
}
