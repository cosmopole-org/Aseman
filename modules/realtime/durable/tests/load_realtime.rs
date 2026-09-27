//! A708 release-capacity probe. It is skipped unless explicitly enabled because its
//! measured result belongs to one candidate deployment, not to a developer laptop.

use std::str::FromStr;
use std::time::Instant;

use aseman_domain::Uuid;
use aseman_domain::realtime::{Event, RetentionClass};
use aseman_ports::realtime::{EventLog, Outbox, Publication};
use aseman_realtime_durable::PostgresRealtime;
use postgres::{Client, Config, NoTls};

const EVENTS: u64 = 2_000;
const MINIMUM_EVENTS_PER_SECOND: f64 = 100.0;

fn publication(stream: &str, creature: Uuid, sequence: u64) -> Publication {
    Publication {
        event: Event {
            id: Uuid::now_v7(),
            stream: stream.to_owned(),
            creature_id: creature,
            kind: "load.tick".to_owned(),
            producer: "a1002".to_owned(),
            sequence,
            at_millis: i64::try_from(sequence).unwrap_or(i64::MAX),
            payload_digest: format!("sha256:{}", "d".repeat(64)),
            retention: RetentionClass::Transient,
            version: "1".to_owned(),
            idempotency_key: Some(format!("load-{sequence}")),
        },
        payload: vec![0x5a; 2_048],
    }
}

#[test]
fn realtime_meets_the_release_capacity_floor_without_loss() {
    let integration = aseman_config::IntegrationTestConfig::from_process();
    if !integration.run_load_tests {
        eprintln!("ASEMAN_RUN_LOAD_TESTS is absent; skipping realtime load test");
        return;
    }
    let admin_uri = integration
        .postgres_url
        .expect("ASEMAN_TEST_POSTGRES_URL is required for realtime load");
    let database = format!("aseman_realtime_load_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);
    let realtime = PostgresRealtime::connect_config(config, 16).unwrap();
    realtime.migrate().unwrap();

    let stream = format!("load/{database}");
    let creature = Uuid::now_v7();
    let started = Instant::now();
    for sequence in 1..=EVENTS {
        realtime
            .append(&publication(&stream, creature, sequence))
            .unwrap();
    }
    let mut published = 0_u64;
    while published < EVENTS {
        let claim = realtime.claim("load-publisher", 100, i64::MAX).unwrap();
        assert!(!claim.events.is_empty(), "outbox lost unpublished events");
        let ids = claim
            .events
            .iter()
            .map(|item| item.event.id)
            .collect::<Vec<_>>();
        realtime.complete("load-publisher", &ids).unwrap();
        published += u64::try_from(ids.len()).unwrap();
    }
    let elapsed = started.elapsed().as_secs_f64();
    let rate = EVENTS as f64 / elapsed.max(f64::EPSILON);
    assert!(
        rate >= MINIMUM_EVENTS_PER_SECOND,
        "{rate:.1} events/s is below the {MINIMUM_EVENTS_PER_SECOND:.1} release floor"
    );
    assert_eq!(
        realtime.read(&stream, 0, EVENTS as usize).unwrap().len(),
        EVENTS as usize
    );
    assert!(realtime.dead_letters(1).unwrap().is_empty());
    eprintln!("A708 realtime load: {EVENTS} events in {elapsed:.3}s ({rate:.1} events/s)");

    drop(realtime);
    admin
        .batch_execute(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .unwrap();
}
