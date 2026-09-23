//! A707 durable realtime on live PostgreSQL: the event log, checkpoints, and the
//! transactional outbox pass their conformance suites, an event and the change that
//! caused it share a transaction, and a worker that dies does not hold its claim.

use std::str::FromStr;

use aseman_domain::Uuid;
use aseman_domain::realtime::{Event, RetentionClass};
use aseman_ports::conformance::realtime::{check_checkpoints, check_event_log, check_outbox};
use aseman_ports::realtime::{EventLog, Outbox, Publication};
use aseman_storage_postgres::realtime::PostgresRealtime;
use postgres::{Client, Config, NoTls};

fn publication(stream: &str, creature: Uuid, sequence: u64, at_millis: i64) -> Publication {
    Publication {
        event: Event {
            id: Uuid::now_v7(),
            stream: stream.to_owned(),
            creature_id: creature,
            kind: "live.tick".to_owned(),
            producer: "live".to_owned(),
            sequence,
            at_millis,
            payload_digest: format!("sha256:{}", "d".repeat(64)),
            retention: RetentionClass::Transient,
            version: "1".to_owned(),
            idempotency_key: None,
        },
        payload: b"payload".to_vec(),
    }
}

#[test]
fn live_realtime_passes_its_conformance_suites() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping realtime test");
        return;
    };
    let database = format!("aseman_realtime_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);

    let realtime = PostgresRealtime::connect_config(config, 8).unwrap();
    realtime.migrate().unwrap();
    // Idempotent: every start runs every migration.
    realtime.migrate().unwrap();

    check_event_log(&realtime, "conformance/log");
    check_checkpoints(&realtime, "consumer-one", "conformance/log");
    check_outbox(&realtime, &realtime, "conformance/outbox");

    // An event and the change that caused it share a transaction, so an event that
    // exists is always one the outbox will publish.
    let creature = Uuid::now_v7();
    realtime
        .append(&publication("transactional", creature, 1, 1_000))
        .unwrap();
    let claim = realtime.claim("worker", 10, 60_000).unwrap();
    assert!(
        claim
            .events
            .iter()
            .any(|item| item.event.stream == "transactional"),
        "an appended event is in the outbox without a second write"
    );
    realtime
        .release(
            "worker",
            &claim
                .events
                .iter()
                .map(|item| item.event.id)
                .collect::<Vec<_>>(),
        )
        .unwrap();

    // A worker that dies holds nothing: its claim expires and another worker takes it.
    let held = realtime.claim("worker-gone", 10, 1).unwrap();
    assert!(!held.events.is_empty());
    let taken = realtime.claim("worker-next", 10, 60_000).unwrap();
    assert!(
        held.events.iter().all(|item| taken
            .events
            .iter()
            .any(|other| other.event.id == item.event.id)),
        "an expired claim is available again: a dead worker must not hold the queue"
    );

    // Retention removes what it promised to, and keeps what it promised to keep.
    let mut durable = publication("retained", creature, 1, 0);
    durable.event.retention = RetentionClass::Durable;
    realtime.append(&durable).unwrap();
    realtime
        .append(&publication("retained", creature, 2, 0))
        .unwrap();
    let now = 8 * 24 * 60 * 60 * 1000;
    realtime.purge_expired(now).unwrap();
    let left = realtime.read("retained", 0, 10).unwrap();
    assert_eq!(left.len(), 1, "the transient event is gone");
    assert_eq!(
        left[0].event.retention,
        RetentionClass::Durable,
        "an audit trail is kept until a retention decision removes it"
    );

    drop(realtime);
    admin
        .batch_execute(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .unwrap();
}
