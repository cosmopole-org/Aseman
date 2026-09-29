//! The Phase 7 gate, live: two independently administered Aseman clusters.
//!
//! Each cluster has its own database, its own directory, and its own envelope guard —
//! nothing is shared but the wire. A permitted operation crosses and is executed; a
//! forbidden one **fails at the destination**, whatever the source believed; a replay
//! is refused; and a retry is answered from the record rather than executed twice.

use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};

use aseman_application::federation::{Refusal, ServeFederatedRequest, Served};
use aseman_domain::Uuid;
use aseman_domain::authority::DecisionReason;
use aseman_domain::federation::{Envelope, FederationError, NodeDescriptor};
use aseman_federation_http::PostgresFederation;
use aseman_policy_native::RegistryPolicy;
use aseman_ports::ClockPort;
use aseman_ports::federation::{Directory, EnvelopeGuard};
use postgres::{Client, Config, NoTls};

/// A clock the test moves by hand, so expiry is exercised rather than waited for.
struct Fixed(std::sync::Mutex<i64>);

impl ClockPort for Fixed {
    fn unix_millis(&self) -> i64 {
        *self.0.lock().unwrap()
    }
}

impl Fixed {
    fn set(&self, millis: i64) {
        *self.0.lock().unwrap() = millis;
    }
}

struct Cluster {
    node_id: Uuid,
    federation: PostgresFederation,
    database: String,
}

fn descriptor(node: Uuid, sequence: u64) -> NodeDescriptor {
    NodeDescriptor {
        node_id: node,
        key_epoch: 1,
        keys: vec!["key".to_owned()],
        federation_endpoint: format!("https://{node}.invalid/federation"),
        client_endpoint: format!("https://{node}.invalid"),
        contracts: vec!["a501/1".to_owned()],
        runtimes: vec!["docker".to_owned()],
        sequence,
        expires_at_millis: 10_000_000,
        revoked_epochs: Vec::new(),
    }
}

fn envelope(from: &Cluster, to: &Cluster, action: &str, target: &str, nonce: &str) -> Envelope {
    Envelope {
        request_id: Uuid::now_v7(),
        source_node: from.node_id,
        destination_node: to.node_id,
        subject: format!("workload:{}", Uuid::now_v7()),
        target: target.to_owned(),
        action: action.to_owned(),
        payload_digest: format!("sha256:{}", "f".repeat(64)),
        issued_at_millis: 1_000,
        expires_at_millis: 31_000,
        nonce: nonce.to_owned(),
        hop_limit: 2,
        version: "1".to_owned(),
    }
}

fn build(admin_uri: &str, name: &str) -> Cluster {
    let database = format!("aseman_fed_{name}_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(admin_uri).unwrap();
    config.dbname(&database);
    let node_id = Uuid::now_v7();
    let federation = PostgresFederation::connect_config(config, 4, node_id).unwrap();
    federation.migrate().unwrap();
    Cluster {
        node_id,
        federation,
        database,
    }
}

#[test]
fn two_clusters_federate_and_the_destination_decides() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping federation gate test");
        return;
    };
    let one = build(&admin_uri, "one");
    let two = build(&admin_uri, "two");
    assert_ne!(one.node_id, two.node_id);

    // Each cluster records its own descriptor and learns the other's. Nothing else is
    // shared: separate databases, separate directories, separate guards.
    for cluster in [&one, &two] {
        cluster
            .federation
            .record_node(&descriptor(cluster.node_id, 1))
            .unwrap();
    }
    one.federation
        .record_node(&descriptor(two.node_id, 1))
        .unwrap();
    two.federation
        .record_node(&descriptor(one.node_id, 1))
        .unwrap();

    let policy = RegistryPolicy::compiled("gate").unwrap();
    let clock = Fixed(std::sync::Mutex::new(2_000));
    let executed = AtomicUsize::new(0);

    let destination = ServeFederatedRequest {
        directory: &two.federation,
        guard: &two.federation,
        policy: &policy,
        clock: &clock,
        node_id: two.node_id,
    };
    let run = |envelope: &Envelope| {
        destination.serve(envelope, |_| {
            executed.fetch_add(1, Ordering::SeqCst);
            Ok("{\"ok\":true}".to_owned())
        })
    };

    // Permitted: a public read crosses the federation and is executed.
    let permitted = envelope(&one, &two, "node.diagnostics.read", "node:two", "n-1");
    assert_eq!(
        run(&permitted).unwrap(),
        Served::Executed("{\"ok\":true}".to_owned())
    );
    assert_eq!(executed.load(Ordering::SeqCst), 1);

    // A retry carries the same request id and is answered from the record. Nothing is
    // executed a second time.
    assert_eq!(
        run(&permitted).unwrap(),
        Served::Replayed("{\"ok\":true}".to_owned())
    );
    assert_eq!(
        executed.load(Ordering::SeqCst),
        1,
        "a retry must not repeat the effect"
    );

    // Forbidden: a relational rule cannot match, because an envelope establishes no
    // relational fact by arriving. It fails at the destination.
    let forbidden = envelope(
        &one,
        &two,
        "creature.update",
        &format!("creature:{}", Uuid::now_v7()),
        "n-2",
    );
    match run(&forbidden).unwrap() {
        Served::Refused(Refusal::Denied(reason)) => assert!(
            matches!(
                reason,
                DecisionReason::ConditionNotMet | DecisionReason::SubjectNotAllowed
            ),
            "denied for a policy reason: {reason:?}"
        ),
        other => panic!("a relational action must fail at the destination: {other:?}"),
    }
    assert_eq!(executed.load(Ordering::SeqCst), 1);

    // An action the registry does not know is denied, not guessed at.
    let unknown = envelope(&one, &two, "not.a.real.action", "node:two", "n-3");
    assert_eq!(
        run(&unknown).unwrap(),
        Served::Refused(Refusal::Denied(DecisionReason::UnknownAction))
    );

    // A replayed envelope — same nonce, fresh request id — is refused.
    let mut replayed = envelope(&one, &two, "node.diagnostics.read", "node:two", "n-1");
    replayed.request_id = Uuid::now_v7();
    assert_eq!(
        run(&replayed).unwrap(),
        Served::Refused(Refusal::Envelope(FederationError::Replayed))
    );
    assert_eq!(executed.load(Ordering::SeqCst), 1);

    // An envelope for the other cluster is refused here, however it arrived.
    let misaddressed = envelope(&two, &one, "node.diagnostics.read", "node:one", "n-4");
    assert_eq!(
        run(&misaddressed).unwrap(),
        Served::Refused(Refusal::Envelope(FederationError::WrongDestination))
    );

    // A peer this cluster does not know is refused before its subject is considered.
    let stranger = Cluster {
        node_id: Uuid::now_v7(),
        federation: PostgresFederation::connect_config(
            Config::from_str(&admin_uri).unwrap(),
            1,
            Uuid::now_v7(),
        )
        .unwrap(),
        database: String::new(),
    };
    let unknown_peer = envelope(&stranger, &two, "node.diagnostics.read", "node:two", "n-5");
    assert_eq!(
        run(&unknown_peer).unwrap(),
        Served::Refused(Refusal::UnknownPeer)
    );

    // Expiry is the destination's own clock, not the source's claim.
    clock.set(31_000);
    let stale = envelope(&one, &two, "node.diagnostics.read", "node:two", "n-6");
    assert_eq!(
        run(&stale).unwrap(),
        Served::Refused(Refusal::Envelope(FederationError::Expired))
    );
    assert_eq!(
        executed.load(Ordering::SeqCst),
        1,
        "only the one permitted request ever executed"
    );

    // The source cluster's own guard saw none of this: the clusters share no state.
    assert!(
        one.federation
            .recorded_answer(permitted.request_id)
            .unwrap()
            .is_none(),
        "the destination's record is its own"
    );

    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    drop(one.federation);
    drop(two.federation);
    drop(stranger.federation);
    for database in [one.database, two.database] {
        admin
            .batch_execute(&format!("DROP DATABASE {database} WITH (FORCE)"))
            .unwrap();
    }
}
