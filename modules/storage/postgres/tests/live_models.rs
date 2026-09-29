//! The PostgreSQL plugin passes the storage provider conformance suite (ADR 0036),
//! on one database and on a two-shard cluster.

use aseman_storage::Storage;
use aseman_storage::provider::{ProviderPlugin, ProviderSettings};
use aseman_storage::schema::Schema;
use aseman_storage_postgres::plugin::PostgresPlugin;
use postgres::{Client, NoTls};

fn retarget(url: &str, database: &str) -> String {
    let slash = url.rfind('/').unwrap();
    format!("{}/{database}", &url[..slash])
}

fn database(admin: &str, label: &str) -> (String, String) {
    let name = format!(
        "aseman_models_{label}_{}",
        &uuid::Uuid::now_v7().simple().to_string()[20..]
    );
    Client::connect(admin, NoTls)
        .unwrap()
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .unwrap();
    (name.clone(), retarget(admin, &name))
}

fn drop_database(admin: &str, name: &str) {
    let _ = Client::connect(admin, NoTls)
        .unwrap()
        .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"));
}

fn settings(url: String, shard_map: Option<String>) -> ProviderSettings {
    let mut settings = ProviderSettings::embedded(std::env::temp_dir()).unwrap();
    settings.database_url = Some(url);
    settings.shard_map = shard_map;
    settings.max_connections = 4;
    settings
}

#[test]
fn postgres_passes_the_storage_provider_conformance_suite() {
    let Some(admin) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping disposable PostgreSQL test");
        return;
    };
    let (name, url) = database(&admin, "single");
    let provider = PostgresPlugin.open(&settings(url, None)).unwrap();
    aseman_storage::conformance::storage_provider(&Storage::new(
        provider,
        Schema::catalog().unwrap(),
    ));
    drop_database(&admin, &name);
}

#[test]
fn a_sharded_postgres_passes_the_storage_provider_conformance_suite() {
    let Some(admin) = aseman_config::IntegrationTestConfig::from_process().postgres_shards_url
    else {
        eprintln!("ASEMAN_TEST_POSTGRES_SHARDS_URL is absent; skipping the sharding suite");
        return;
    };
    let (first, first_url) = database(&admin, "a");
    let (second, second_url) = database(&admin, "b");
    let map = serde_json::json!({
        "version": 1,
        "home": "a",
        "read_from_replicas": false,
        "shards": [
            {"name": "a", "primary": first_url, "replicas": []},
            {"name": "b", "primary": second_url, "replicas": []}
        ]
    })
    .to_string();
    let provider = PostgresPlugin
        .open(&settings(first_url.clone(), Some(map)))
        .unwrap();
    aseman_storage::conformance::storage_provider(&Storage::new(
        provider,
        Schema::catalog().unwrap(),
    ));
    drop_database(&admin, &first);
    drop_database(&admin, &second);
}
