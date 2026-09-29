//! The legacy signal history kept in PostgreSQL (`ASEMAN_SIGNAL_LOG_PROVIDER=postgres`
//! before ADR 0036) reads back whole, by store then time, for the storage migration.
//!
//! Needs `ASEMAN_TEST_POSTGRES_URL` (an administrative URL); skips without it.

use aseman_storage_rocksdb::{LegacySignalLogSource, read_legacy_signals};
use postgres::{Client, NoTls};

#[test]
fn live_legacy_signal_rows_read_back_for_migration() {
    let Some(admin) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping legacy signal log test");
        return;
    };
    let database = format!("aseman_signal_log_{}", uuid::Uuid::now_v7().simple());
    let mut client = Client::connect(&admin, NoTls).unwrap();
    client
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let (base, _) = admin.rsplit_once('/').unwrap();
    let url = format!("{base}/{database}");
    let source = LegacySignalLogSource::Postgres { url: url.clone() };
    // A database that never served signals has none.
    assert!(read_legacy_signals(&source).unwrap().is_empty());
    let mut legacy = Client::connect(&url, NoTls).unwrap();
    legacy
        .batch_execute(
            "CREATE SCHEMA aseman_legacy_log;
             CREATE TABLE aseman_legacy_log.signals(id text PRIMARY KEY, store_id text NOT NULL,
               user_id text NOT NULL, data text NOT NULL, tags text, time bigint NOT NULL,
               edited boolean NOT NULL DEFAULT false);
             INSERT INTO aseman_legacy_log.signals VALUES
               ('s2', 'store-b', 'u1', 'two', '|kind=a|', 20, false),
               ('s1', 'store-a', 'u1', 'one', NULL, 30, true),
               ('s0', 'store-a', 'u2', 'zero', '|x|', 10, false);",
        )
        .unwrap();
    let rows = read_legacy_signals(&source).unwrap();
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["s0", "s1", "s2"]
    );
    assert_eq!((rows[1].encoded_tags.as_str(), rows[1].edited), ("", true));
    drop(legacy);
    client
        .batch_execute(&format!("DROP DATABASE {database}"))
        .unwrap();
}
