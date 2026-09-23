//! The A704/A705 federation conformance kit.
//!
//! Every directory and envelope guard runs this. The cases are the ones that decide
//! whether a peer can lie to this node: a replayed descriptor, a replayed envelope, and
//! a retry that must not execute twice.

use aseman_domain::Uuid;
use aseman_domain::federation::{Envelope, NodeDescriptor, WorkloadDescriptor};

use crate::PortError;
use crate::federation::{Directory, EnvelopeGuard};

fn node_descriptor(node: Uuid, sequence: u64, epoch: u32, expires: i64) -> NodeDescriptor {
    NodeDescriptor {
        node_id: node,
        key_epoch: epoch,
        keys: vec![format!("key-{epoch}")],
        federation_endpoint: format!("https://{node}.invalid/federation"),
        client_endpoint: format!("https://{node}.invalid"),
        contracts: vec!["a501/1".to_owned()],
        runtimes: vec!["docker".to_owned()],
        sequence,
        expires_at_millis: expires,
        revoked_epochs: Vec::new(),
    }
}

/// Run the directory contract. `now_millis` must be before `100_000`.
///
/// # Panics
///
/// Panics when the provider deviates from the port contract.
pub fn check_directory(directory: &dyn Directory, now_millis: i64) {
    let peer = Uuid::now_v7();
    assert!(
        directory.node(peer, now_millis).expect("read").is_none(),
        "the suite needs a node the directory has not learned"
    );

    directory
        .record_node(&node_descriptor(peer, 5, 2, 100_000))
        .expect("record");
    assert_eq!(
        directory
            .node(peer, now_millis)
            .expect("read")
            .expect("recorded")
            .key_epoch,
        2
    );

    // A replayed older descriptor cannot un-rotate a key.
    assert_eq!(
        directory.record_node(&node_descriptor(peer, 4, 1, 100_000)),
        Err(PortError::Conflict),
        "an older sequence is refused"
    );
    assert_eq!(
        directory.record_node(&node_descriptor(peer, 5, 1, 100_000)),
        Err(PortError::Conflict),
        "the same sequence is refused: it must rise"
    );
    assert_eq!(
        directory
            .node(peer, now_millis)
            .expect("read")
            .expect("recorded")
            .key_epoch,
        2,
        "a refused descriptor does not roll the cache back"
    );

    // A node may not publish keys it has itself revoked.
    let mut self_revoked = node_descriptor(Uuid::now_v7(), 1, 4, 100_000);
    self_revoked.revoked_epochs = vec![4];
    assert!(
        matches!(
            directory.record_node(&self_revoked),
            Err(PortError::Denied(_))
        ),
        "a self-revoked epoch is refused"
    );

    // An expired descriptor is not served: the home node is authoritative.
    assert!(
        directory.node(peer, 100_000).expect("read").is_none(),
        "a cache does not serve what it should have let go"
    );

    // Workload descriptors move forward by revision.
    let workload = Uuid::now_v7();
    let descriptor = WorkloadDescriptor {
        workload_id: workload,
        home_node: peer,
        home_endpoint: "https://home.invalid".to_owned(),
        public_key: "key".to_owned(),
        revision: 3,
        expires_at_millis: 100_000,
    };
    directory.record_workload(&descriptor).expect("record");
    let mut older = descriptor.clone();
    older.revision = 2;
    assert_eq!(
        directory.record_workload(&older),
        Err(PortError::Conflict),
        "an older revision is refused"
    );
    let mut newer = descriptor;
    newer.revision = 4;
    newer.public_key = "rotated".to_owned();
    directory.record_workload(&newer).expect("record");
    assert_eq!(
        directory
            .workload(workload, now_millis)
            .expect("read")
            .expect("recorded")
            .public_key,
        "rotated"
    );
}

/// Run the envelope guard contract.
///
/// # Panics
///
/// Panics when the provider deviates from the port contract.
pub fn check_envelope_guard(guard: &dyn EnvelopeGuard) {
    let source = Uuid::now_v7();
    let envelope = Envelope {
        request_id: Uuid::now_v7(),
        source_node: source,
        destination_node: Uuid::now_v7(),
        subject: "workload:1111".to_owned(),
        target: "workload:2222".to_owned(),
        action: "workload.signal".to_owned(),
        payload_digest: format!("sha256:{}", "a".repeat(64)),
        issued_at_millis: 1_000,
        expires_at_millis: 31_000,
        nonce: format!("nonce-{}", Uuid::now_v7()),
        hop_limit: 2,
        version: "1".to_owned(),
    };

    assert!(guard.remember_nonce(&envelope).expect("remember"));
    assert!(
        !guard.remember_nonce(&envelope).expect("remember"),
        "a repeated nonce is a replay"
    );

    // Nonces are per source node: two peers may choose the same value.
    let mut other = envelope.clone();
    other.source_node = Uuid::now_v7();
    assert!(
        guard.remember_nonce(&other).expect("remember"),
        "another source may use the same nonce value"
    );

    // A retry is answered from the record rather than executed again.
    assert!(
        guard
            .recorded_answer(envelope.request_id)
            .expect("read")
            .is_none()
    );
    guard
        .record_answer(
            envelope.request_id,
            "{\"ok\":true}",
            envelope.expires_at_millis,
        )
        .expect("record");
    assert_eq!(
        guard
            .recorded_answer(envelope.request_id)
            .expect("read")
            .as_deref(),
        Some("{\"ok\":true}")
    );

    // The first answer stands: a racing retry must not replace what a caller may
    // already have received.
    guard
        .record_answer(
            envelope.request_id,
            "{\"ok\":false}",
            envelope.expires_at_millis,
        )
        .expect("record");
    assert_eq!(
        guard
            .recorded_answer(envelope.request_id)
            .expect("read")
            .as_deref(),
        Some("{\"ok\":true}"),
        "the recorded answer is the one that was given"
    );

    // Purging drops what could no longer be accepted anyway.
    let purged = guard.purge_expired(40_000).expect("purge");
    assert!(purged >= 2, "nonces and answers are both purged: {purged}");
    assert!(
        guard
            .recorded_answer(envelope.request_id)
            .expect("read")
            .is_none()
    );
    assert!(
        guard.remember_nonce(&envelope).expect("remember"),
        "a purged nonce may be used again: its envelope is long expired"
    );
}
