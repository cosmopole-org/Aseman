use super::*;

fn node(byte: u8) -> Uuid {
    Uuid::from_bytes([byte; 16])
}

fn envelope() -> Envelope {
    Envelope {
        request_id: node(9),
        source_node: node(1),
        destination_node: node(2),
        subject: "workload:1111".to_owned(),
        target: "workload:2222".to_owned(),
        action: "workload.signal".to_owned(),
        payload_digest: format!("sha256:{}", "a".repeat(64)),
        issued_at_millis: 1_000,
        expires_at_millis: 31_000,
        nonce: "nonce-one".to_owned(),
        hop_limit: 2,
        version: "1".to_owned(),
    }
}

#[test]
fn a_well_formed_envelope_is_accepted_by_its_destination() {
    assert!(accept(&envelope(), node(2), 2_000, false).is_ok());
}

#[test]
fn an_envelope_addressed_elsewhere_is_refused_whoever_hands_it_over() {
    assert_eq!(
        accept(&envelope(), node(3), 2_000, false),
        Err(FederationError::WrongDestination)
    );
}

#[test]
fn a_repeated_nonce_is_a_replay() {
    assert_eq!(
        accept(&envelope(), node(2), 2_000, true),
        Err(FederationError::Replayed)
    );
}

#[test]
fn an_envelope_may_not_outlive_the_federation_maximum() {
    let mut envelope = envelope();
    envelope.expires_at_millis = envelope.issued_at_millis + MAX_LIFETIME_MILLIS + 1;
    assert_eq!(
        accept(&envelope, node(2), 2_000, false),
        Err(FederationError::LifetimeTooLong),
        "a long-lived envelope is a replay waiting to happen"
    );
    envelope.expires_at_millis = envelope.issued_at_millis + MAX_LIFETIME_MILLIS;
    assert!(accept(&envelope, node(2), 2_000, false).is_ok());
}

#[test]
fn an_expired_envelope_is_refused_by_the_destinations_own_clock() {
    let envelope = envelope();
    assert!(accept(&envelope, node(2), 30_999, false).is_ok());
    assert_eq!(
        accept(&envelope, node(2), 31_000, false),
        Err(FederationError::Expired)
    );
}

#[test]
fn a_hop_limit_above_the_maximum_is_refused_and_a_loop_cannot_outlive_it() {
    let mut too_many = envelope();
    too_many.hop_limit = MAX_HOP_LIMIT + 1;
    assert_eq!(
        accept(&too_many, node(2), 2_000, false),
        Err(FederationError::HopLimitTooHigh)
    );

    // Forwarding always decreases, and stops.
    let mut hop = envelope();
    hop.hop_limit = 2;
    let once = forward(&hop).expect("one hop");
    assert_eq!(once.hop_limit, 1);
    let twice = forward(&once).expect("two hops");
    assert_eq!(twice.hop_limit, 0);
    assert_eq!(
        forward(&twice),
        Err(FederationError::HopLimitReached),
        "a routing loop ends here"
    );
}

#[test]
fn an_envelope_must_carry_a_real_payload_digest() {
    let mut envelope = envelope();
    for bad in [
        "",
        "sha256:short",
        "md5:abc",
        &format!("sha256:{}", "z".repeat(64)),
    ] {
        envelope.payload_digest = bad.to_owned();
        assert_eq!(
            accept(&envelope, node(2), 2_000, false),
            Err(FederationError::InvalidDigest),
            "{bad:?}"
        );
    }
}

#[test]
fn an_envelope_must_name_a_subject_a_target_and_an_action() {
    for blank in ["subject", "target", "action"] {
        let mut envelope = envelope();
        match blank {
            "subject" => envelope.subject.clear(),
            "target" => envelope.target.clear(),
            _ => envelope.action.clear(),
        }
        assert_eq!(
            accept(&envelope, node(2), 2_000, false),
            Err(FederationError::Incomplete),
            "a blank {blank}"
        );
    }
}

#[test]
fn a_node_does_not_federate_with_itself() {
    let mut envelope = envelope();
    envelope.source_node = envelope.destination_node;
    assert_eq!(
        accept(&envelope, node(2), 2_000, false),
        Err(FederationError::SelfAddressed)
    );
}

#[test]
fn an_unknown_envelope_version_is_refused_rather_than_guessed() {
    let mut envelope = envelope();
    envelope.version = "2".to_owned();
    assert_eq!(
        accept(&envelope, node(2), 2_000, false),
        Err(FederationError::UnknownVersion)
    );
}

fn descriptor(sequence: u64, epoch: u32) -> NodeDescriptor {
    NodeDescriptor {
        node_id: node(1),
        key_epoch: epoch,
        keys: vec!["key".to_owned()],
        federation_endpoint: "https://one.invalid/federation".to_owned(),
        client_endpoint: "https://one.invalid".to_owned(),
        contracts: vec!["a501/1".to_owned()],
        runtimes: vec!["docker".to_owned()],
        sequence,
        expires_at_millis: 100_000,
        revoked_epochs: Vec::new(),
    }
}

#[test]
fn a_replayed_older_descriptor_cannot_un_rotate_a_key() {
    let cached = descriptor(5, 2);
    assert!(
        !accepts_node_descriptor(Some(&cached), &descriptor(4, 1)),
        "an older sequence is refused"
    );
    assert!(
        !accepts_node_descriptor(Some(&cached), &descriptor(5, 1)),
        "the same sequence is refused: the sequence must rise"
    );
    assert!(accepts_node_descriptor(Some(&cached), &descriptor(6, 3)));
    assert!(accepts_node_descriptor(None, &descriptor(1, 1)));
}

#[test]
fn a_descriptor_that_names_its_own_epoch_as_revoked_is_refused() {
    let mut revoked = descriptor(9, 3);
    revoked.revoked_epochs = vec![3];
    assert!(
        !accepts_node_descriptor(None, &revoked),
        "a node cannot publish keys it has itself revoked"
    );
}

#[test]
fn a_workload_descriptor_moves_forward_by_revision_and_expires() {
    let cached = WorkloadDescriptor {
        workload_id: node(7),
        home_node: node(1),
        home_endpoint: "https://one.invalid".to_owned(),
        public_key: "key".to_owned(),
        revision: 3,
        expires_at_millis: 10_000,
    };
    let mut newer = cached.clone();
    newer.revision = 4;
    assert!(accepts_workload_descriptor(Some(&cached), &newer));
    let mut older = cached.clone();
    older.revision = 2;
    assert!(!accepts_workload_descriptor(Some(&cached), &older));
    assert!(descriptor_is_fresh(cached.expires_at_millis, 9_999));
    assert!(!descriptor_is_fresh(cached.expires_at_millis, 10_000));
}
