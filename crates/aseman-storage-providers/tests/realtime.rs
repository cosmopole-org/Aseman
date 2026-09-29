//! A707 durable realtime on the storage module, on every provider: the event log,
//! checkpoints, and the transactional outbox pass their conformance suites, an event
//! and the change that caused it share a transaction, and a worker that dies does not
//! hold its claim. A708's release-capacity probe runs on PostgreSQL when enabled.

mod support;

use std::time::Instant;

use aseman_capsule::realtime::StorageRealtime;
use aseman_domain::Uuid;
use aseman_domain::realtime::{Event, RetentionClass};
use aseman_ports::conformance::realtime::{check_checkpoints, check_event_log, check_outbox};
use aseman_ports::realtime::{EventLog, Outbox, Publication};

const EVENTS: u64 = 2_000;
const MINIMUM_EVENTS_PER_SECOND: f64 = 100.0;

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

fn load_publication(stream: &str, creature: Uuid, sequence: u64) -> Publication {
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
fn realtime_passes_its_conformance_suites() {
    for store in support::stores() {
        eprintln!("realtime on {}", store.name);
        let realtime = StorageRealtime::new(store.storage.clone());
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
    }
}

/// The measured result belongs to one candidate deployment, not to a developer laptop,
/// so it is skipped unless explicitly enabled.
#[test]
fn realtime_meets_the_release_capacity_floor_without_loss() {
    let integration = aseman_config::IntegrationTestConfig::from_process();
    if !integration.run_load_tests {
        eprintln!("ASEMAN_RUN_LOAD_TESTS is absent; skipping realtime load test");
        return;
    }
    let store =
        support::postgres(16).expect("ASEMAN_TEST_POSTGRES_URL is required for realtime load");
    let realtime = StorageRealtime::new(store.storage.clone());
    let stream = "load".to_owned();
    let creature = Uuid::now_v7();
    let started = Instant::now();
    for sequence in 1..=EVENTS {
        realtime
            .append(&load_publication(&stream, creature, sequence))
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
}
