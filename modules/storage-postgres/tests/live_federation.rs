//! A704/A705 federation records on live PostgreSQL: descriptors only move forward, an
//! expired descriptor is not served, a repeated nonce is a replay, and a retry is
//! answered from the record rather than executed again.

use std::str::FromStr;

use aseman_domain::Uuid;
use aseman_domain::federation::{Envelope, NodeDescriptor, WorkloadDescriptor};
use aseman_ports::PortError;
use aseman_ports::federation::{Directory, EnvelopeGuard};
use aseman_storage_postgres::federation::PostgresFederation;
use postgres::{Client, Config, NoTls};

fn node_descriptor(node: Uuid, sequence: u64, epoch: u32, expires: i64) -> NodeDescriptor {
    NodeDescriptor {
        node_id: node,
        key_epoch: epoch,
        keys: vec!["key".to_owned()],
        federation_endpoint: "https://one.invalid/federation".to_owned(),
        client_endpoint: "https://one.invalid".to_owned(),
        contracts: vec!["a501/1".to_owned()],
        runtimes: vec!["docker".to_owned()],
        sequence,
        expires_at_millis: expires,
        revoked_epochs: Vec::new(),
    }
}

fn envelope(source: Uuid, nonce: &str) -> Envelope {
    Envelope {
        request_id: Uuid::now_v7(),
        source_node: source,
        destination_node: Uuid::now_v7(),
        subject: "workload:1111".to_owned(),
        target: "workload:2222".to_owned(),
        action: "workload.signal".to_owned(),
        payload_digest: format!("sha256:{}", "e".repeat(64)),
        issued_at_millis: 1_000,
        expires_at_millis: 31_000,
        nonce: nonce.to_owned(),
        hop_limit: 2,
        version: "1".to_owned(),
    }
}

#[test]
fn live_federation_records_only_move_forward() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping federation test");
        return;
    };
    let database = format!("aseman_federation_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);

    let own = Uuid::now_v7();
    let federation = PostgresFederation::connect_config(config, 4, own).unwrap();
    federation.migrate().unwrap();
    federation.migrate().unwrap();

    // The port conformance kit: every provider must behave this way.
    aseman_ports::conformance::federation::check_directory(&federation, 1_000);
    aseman_ports::conformance::federation::check_envelope_guard(&federation);

    // A node with no descriptor of its own is a configuration failure, not an empty
    // cache.
    assert_eq!(federation.own_node(), Err(PortError::NotFound));

    let peer = Uuid::now_v7();
    federation
        .record_node(&node_descriptor(peer, 5, 2, 100_000))
        .unwrap();
    assert_eq!(federation.node(peer, 1_000).unwrap().unwrap().key_epoch, 2);

    // A replayed older descriptor cannot un-rotate a key.
    assert_eq!(
        federation.record_node(&node_descriptor(peer, 4, 1, 100_000)),
        Err(PortError::Conflict)
    );
    assert_eq!(
        federation.record_node(&node_descriptor(peer, 5, 1, 100_000)),
        Err(PortError::Conflict),
        "the same sequence is refused: it must rise"
    );
    assert_eq!(
        federation.node(peer, 1_000).unwrap().unwrap().key_epoch,
        2,
        "a refused descriptor does not roll the cache back"
    );
    federation
        .record_node(&node_descriptor(peer, 6, 3, 100_000))
        .unwrap();
    assert_eq!(federation.node(peer, 1_000).unwrap().unwrap().key_epoch, 3);

    // A node cannot publish keys it has itself revoked.
    let mut self_revoked = node_descriptor(Uuid::now_v7(), 1, 4, 100_000);
    self_revoked.revoked_epochs = vec![4];
    assert!(matches!(
        federation.record_node(&self_revoked),
        Err(PortError::Denied(_))
    ));

    // An expired descriptor is not served: the home node is authoritative.
    assert!(
        federation.node(peer, 100_000).unwrap().is_none(),
        "a cache does not serve what it should have let go"
    );

    // Workload descriptors move forward by revision.
    let workload = Uuid::now_v7();
    let descriptor = WorkloadDescriptor {
        workload_id: workload,
        home_node: peer,
        home_endpoint: "https://one.invalid".to_owned(),
        public_key: "key".to_owned(),
        revision: 3,
        expires_at_millis: 100_000,
    };
    federation.record_workload(&descriptor).unwrap();
    let mut older = descriptor.clone();
    older.revision = 2;
    assert_eq!(federation.record_workload(&older), Err(PortError::Conflict));
    let mut newer = descriptor.clone();
    newer.revision = 4;
    newer.public_key = "rotated".to_owned();
    federation.record_workload(&newer).unwrap();
    assert_eq!(
        federation
            .workload(workload, 1_000)
            .unwrap()
            .unwrap()
            .public_key,
        "rotated"
    );

    // A nonce is remembered once. The second time is a replay.
    let source = Uuid::now_v7();
    let first = envelope(source, "nonce-one");
    assert!(federation.remember_nonce(&first).unwrap());
    assert!(
        !federation.remember_nonce(&first).unwrap(),
        "a repeated nonce is a replay"
    );
    // Another source may use the same nonce value: nonces are per source node.
    let other = envelope(Uuid::now_v7(), "nonce-one");
    assert!(federation.remember_nonce(&other).unwrap());

    // A retry is answered from the record rather than executed again.
    assert!(
        federation
            .recorded_answer(first.request_id)
            .unwrap()
            .is_none()
    );
    federation
        .record_answer(first.request_id, "{\"ok\":true}", 31_000)
        .unwrap();
    assert_eq!(
        federation
            .recorded_answer(first.request_id)
            .unwrap()
            .as_deref(),
        Some("{\"ok\":true}")
    );
    // The first answer stands: a racing retry must not replace what a caller may
    // already have received.
    federation
        .record_answer(first.request_id, "{\"ok\":false}", 31_000)
        .unwrap();
    assert_eq!(
        federation
            .recorded_answer(first.request_id)
            .unwrap()
            .as_deref(),
        Some("{\"ok\":true}")
    );

    // Purging drops what can no longer be accepted anyway.
    let purged = federation.purge_expired(40_000).unwrap();
    assert!(purged >= 3, "nonces and answers are both purged: {purged}");
    assert!(
        federation
            .recorded_answer(first.request_id)
            .unwrap()
            .is_none()
    );
    assert!(
        federation.remember_nonce(&first).unwrap(),
        "a purged nonce can be used again, because its envelope is long expired"
    );

    drop(federation);
    admin
        .batch_execute(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .unwrap();
}
