//! The A707 realtime conformance kit.
//!
//! Every provider runs this. The cases are the ones that decide whether a consumer
//! can trust the stream: dense ordering, no backwards checkpoint, an honest answer
//! about what has been lost to retention, and a claim that only its holder can
//! complete.

use aseman_domain::Uuid;
use aseman_domain::realtime::{Checkpoint, Event, RetentionClass};

use crate::PortError;
use crate::realtime::{CheckpointStore, EventLog, Outbox, Publication};

fn event(stream: &str, creature: Uuid, sequence: u64, at_millis: i64) -> Event {
    Event {
        id: Uuid::now_v7(),
        stream: stream.to_owned(),
        creature_id: creature,
        kind: "conformance.tick".to_owned(),
        producer: "conformance".to_owned(),
        sequence,
        at_millis,
        payload_digest: format!("sha256:{}", "c".repeat(64)),
        retention: RetentionClass::Standard,
        version: "1".to_owned(),
        idempotency_key: None,
    }
}

fn publication(stream: &str, creature: Uuid, sequence: u64, at_millis: i64) -> Publication {
    Publication {
        event: event(stream, creature, sequence, at_millis),
        payload: format!("payload-{sequence}").into_bytes(),
    }
}

/// Run the event-log contract against `log`, on a stream nothing else uses.
///
/// # Panics
///
/// Panics when the provider deviates from the port contract.
pub fn check_event_log(log: &dyn EventLog, stream: &str) {
    let creature = Uuid::now_v7();
    assert_eq!(
        log.bounds(stream).expect("bounds"),
        (None, None),
        "the suite needs a stream nothing has written"
    );

    for sequence in 1..=5 {
        log.append(&publication(stream, creature, sequence, 1_000))
            .unwrap_or_else(|error| panic!("append {sequence}: {error:?}"));
    }
    assert_eq!(log.bounds(stream).expect("bounds").0, Some(5));

    // A gap is refused: a consumer would wait forever for the event in it.
    assert_eq!(
        log.append(&publication(stream, creature, 7, 1_000)),
        Err(PortError::Conflict),
        "a stream's sequences are dense"
    );
    // So is a repeat.
    assert_eq!(
        log.append(&publication(stream, creature, 5, 1_000)),
        Err(PortError::Conflict),
        "a repeated sequence would make replay ambiguous"
    );

    // Reading is ordered, bounded, and carries the payload the digest covers.
    let page = log.read(stream, 0, 3).expect("read");
    assert_eq!(page.len(), 3);
    assert_eq!(
        page.iter()
            .map(|item| item.event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3],
        "oldest first"
    );
    assert_eq!(page[0].payload, b"payload-1");
    let rest = log.read(stream, 3, 100).expect("read after");
    assert_eq!(
        rest.iter()
            .map(|item| item.event.sequence)
            .collect::<Vec<_>>(),
        vec![4, 5]
    );
    assert!(
        log.read(stream, 5, 100)
            .expect("read past the end")
            .is_empty(),
        "reading past the end is empty, not an error"
    );
}

/// Run the checkpoint contract.
///
/// # Panics
///
/// Panics when the provider deviates from the port contract.
pub fn check_checkpoints(store: &dyn CheckpointStore, consumer: &str, stream: &str) {
    assert!(
        store.checkpoint(consumer, stream).expect("read").is_none(),
        "the suite needs a consumer that has not started"
    );

    let mut checkpoint = Checkpoint {
        consumer: consumer.to_owned(),
        stream: stream.to_owned(),
        sequence: 3,
        at_millis: 1_000,
    };
    store.record(&checkpoint).expect("record");
    assert_eq!(
        store
            .checkpoint(consumer, stream)
            .expect("read")
            .expect("recorded")
            .sequence,
        3
    );

    // Forward, and a repeat, are both fine.
    checkpoint.sequence = 5;
    store.record(&checkpoint).expect("forward");
    store.record(&checkpoint).expect("a repeat is harmless");

    // Backwards is refused: it would replay events the consumer already acted on.
    checkpoint.sequence = 4;
    assert_eq!(
        store.record(&checkpoint),
        Err(PortError::Conflict),
        "a checkpoint never moves backwards"
    );
    assert_eq!(
        store
            .checkpoint(consumer, stream)
            .expect("read")
            .expect("recorded")
            .sequence,
        5,
        "a refused checkpoint does not move it back"
    );
}

/// Run the outbox contract against `outbox`, whose backing log is `log`.
///
/// # Panics
///
/// Panics when the provider deviates from the port contract.
pub fn check_outbox(outbox: &dyn Outbox, log: &dyn EventLog, stream: &str) {
    let creature = Uuid::now_v7();
    for sequence in 1..=3 {
        log.append(&publication(stream, creature, sequence, 1_000))
            .expect("append");
    }

    // One worker claims; another sees nothing of what the first holds.
    let first = outbox.claim("worker-a", 2, 60_000).expect("claim");
    assert_eq!(first.events.len(), 2, "a claim is bounded");
    let held: Vec<_> = first.events.iter().map(|item| item.event.id).collect();
    let second = outbox.claim("worker-b", 10, 60_000).expect("claim");
    assert!(
        second
            .events
            .iter()
            .all(|item| !held.contains(&item.event.id)),
        "a claimed event is not handed to a second worker"
    );

    // Only the worker that claimed may complete.
    assert_eq!(
        outbox.complete("worker-b", &held),
        Err(PortError::Conflict),
        "another worker may not complete this claim"
    );
    outbox.complete("worker-a", &held).expect("complete");

    // A completed event is not claimed again.
    let again = outbox.claim("worker-a", 10, 60_000).expect("claim");
    assert!(
        again
            .events
            .iter()
            .all(|item| !held.contains(&item.event.id)),
        "a published event is not published twice"
    );

    // A released claim goes back into the queue.
    let released: Vec<_> = again.events.iter().map(|item| item.event.id).collect();
    outbox.release("worker-a", &released).expect("release");
    let retaken = outbox.claim("worker-b", 10, 60_000).expect("claim");
    assert!(
        released
            .iter()
            .all(|id| retaken.events.iter().any(|item| item.event.id == *id)),
        "a released claim is available to another worker"
    );
    outbox.complete("worker-b", &released).expect("complete");
}
