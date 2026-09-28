//! The PostgreSQL consensus-log storage passes the port contract (ADR 0035).

use aseman_storage_postgres::consensus_log::PostgresConsensusLogStorage;

#[test]
fn postgres_logs_pass_the_consensus_log_contract() {
    let Some(url) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping disposable PostgreSQL test");
        return;
    };
    let storage = PostgresConsensusLogStorage::connect(&url, 2).unwrap();
    let name = format!("conformance-{}", uuid::Uuid::now_v7().simple());
    aseman_ports::conformance::consensus_log::consensus_log(&storage, &name);
}
