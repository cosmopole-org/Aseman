use super::*;

fn creature(byte: u8) -> Uuid {
    Uuid::from_bytes([byte; 16])
}

fn event(sequence: u64) -> Event {
    Event {
        id: Uuid::from_bytes([sequence as u8; 16]),
        stream: "creature/7/signals".to_owned(),
        creature_id: creature(7),
        kind: "store.signal".to_owned(),
        producer: "workload:1111".to_owned(),
        sequence,
        at_millis: 1_000,
        payload_digest: format!("sha256:{}", "b".repeat(64)),
        retention: RetentionClass::Standard,
        version: "1".to_owned(),
        idempotency_key: None,
    }
}

#[test]
fn a_streams_sequences_are_dense_and_start_at_one() {
    assert!(accept_append(&event(1), None).is_ok());
    assert_eq!(
        accept_append(&event(2), None),
        Err(RealtimeError::OutOfOrder { expected: 1 }),
        "a stream starts at 1"
    );
    assert!(accept_append(&event(6), Some(5)).is_ok());
    assert_eq!(
        accept_append(&event(7), Some(5)),
        Err(RealtimeError::OutOfOrder { expected: 6 }),
        "a gap would make a consumer wait for an event that never comes"
    );
    assert_eq!(
        accept_append(&event(5), Some(5)),
        Err(RealtimeError::OutOfOrder { expected: 6 }),
        "a repeat would make replay ambiguous"
    );
}

#[test]
fn an_event_must_carry_a_real_payload_digest_and_a_known_version() {
    let mut bad = event(1);
    bad.payload_digest = "sha256:short".to_owned();
    assert_eq!(accept_append(&bad, None), Err(RealtimeError::InvalidDigest));
    let mut future = event(1);
    future.version = "2".to_owned();
    assert_eq!(
        accept_append(&future, None),
        Err(RealtimeError::UnknownVersion)
    );
    let mut nameless = event(1);
    nameless.stream.clear();
    assert_eq!(
        accept_append(&nameless, None),
        Err(RealtimeError::InvalidStream)
    );
}

#[test]
fn a_checkpoint_never_moves_backwards() {
    assert!(accept_checkpoint(None, 5).is_ok());
    assert!(
        accept_checkpoint(Some(5), 5).is_ok(),
        "a repeat is harmless"
    );
    assert!(accept_checkpoint(Some(5), 6).is_ok());
    assert_eq!(
        accept_checkpoint(Some(5), 4),
        Err(RealtimeError::CheckpointWentBackwards),
        "moving back would replay events the consumer already acted on"
    );
}

#[test]
fn a_consumer_outside_the_retention_window_is_told_to_resync() {
    // The stream still holds everything from sequence 10 onwards.
    assert!(can_replay_from(Some(10), 9), "the next event is 10: fine");
    assert!(can_replay_from(Some(10), 20));
    assert!(
        !can_replay_from(Some(10), 5),
        "the events after 5 are gone; a silently incomplete stream is worse than a resync"
    );
    assert!(can_replay_from(None, 0), "an empty stream has lost nothing");
}

#[test]
fn discoverability_is_not_delivery() {
    let event = event(1);
    assert!(may_deliver(&event, creature(7)));
    assert!(
        !may_deliver(&event, creature(8)),
        "another creature's subscriber sees nothing, however discoverable the producer is"
    );
}

#[test]
fn retention_classes_keep_what_they_promise() {
    let mut transient = event(1);
    transient.retention = RetentionClass::Transient;
    assert!(is_retained(&transient, 1_000 + 60 * 60 * 1000 - 1));
    assert!(!is_retained(&transient, 1_000 + 60 * 60 * 1000));

    let standard = event(1);
    assert!(is_retained(&standard, 1_000 + 24 * 60 * 60 * 1000));
    assert!(!is_retained(&standard, 1_000 + 8 * 24 * 60 * 60 * 1000));

    let mut durable = event(1);
    durable.retention = RetentionClass::Durable;
    assert!(
        is_retained(&durable, i64::MAX),
        "an audit trail is kept until a retention decision removes it"
    );
    assert_eq!(RetentionClass::Durable.millis(), None);
}
