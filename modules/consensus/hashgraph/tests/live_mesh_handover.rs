//! RL-011: a checkpointed finance-consensus handover observed on a live peer mesh.
//!
//! Four Babble validators gossip over real loopback TCP, each with its own
//! `HashgraphConsensusProvider` installed as the application proxy. Finance records
//! submitted through the `ConsensusProvider` port on different peers are ordered by the
//! mesh; every peer must finalize the same records at the same epochs and positions.
//! The outgoing mesh is then checked for handover readiness, a checkpoint is taken at
//! its finalized epoch, the outgoing mesh stops, and a second, independent mesh adopts
//! that checkpoint and keeps
//! ordering on top of it. The outgoing providers keep their history, which is the
//! rollback path: a fresh provider can adopt the same checkpoint again.
//!
//! This binds real TCP ports on 127.0.0.1 and runs by default; it needs no external
//! service.

use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use aseman_application::consensus::{
    AdoptConsensusCheckpoint, CheckHandoverReadiness, FinanceConsensusStatus, SubmitFinanceRecord,
    journal_digest,
};
use aseman_consensus_hashgraph::babble::Babble;
use aseman_consensus_hashgraph::config::Config;
use aseman_consensus_hashgraph::crypto::keys::{generate_ecdsa_key, public_key_hex};
use aseman_consensus_hashgraph::peers::{JSONPeerSet, Peer};
use aseman_consensus_hashgraph::provider::HashgraphConsensusProvider;
use aseman_domain::consensus::{Epoch, Finalized};
use aseman_ports::consensus::ConsensusProvider;

const PEERS: usize = 4;
const DEADLINE: Duration = Duration::from_secs(90);

struct Mesh {
    providers: Vec<Arc<HashgraphConsensusProvider>>,
    engines: Vec<Babble>,
}

impl Mesh {
    fn start(root: &Path, generation: &str) -> Self {
        let ports: Vec<u16> = (0..PEERS)
            .map(|_| {
                TcpListener::bind("127.0.0.1:0")
                    .unwrap()
                    .local_addr()
                    .unwrap()
                    .port()
            })
            .collect();
        let keys: Vec<_> = (0..PEERS).map(|_| generate_ecdsa_key().unwrap()).collect();
        let peers: Vec<Peer> = keys
            .iter()
            .zip(&ports)
            .enumerate()
            .map(|(index, (key, port))| {
                Peer::new(
                    &public_key_hex(key.verifying_key()),
                    &format!("127.0.0.1:{port}"),
                    &format!("{generation}-{index}"),
                )
            })
            .collect();
        let mut providers = Vec::new();
        let mut engines = Vec::new();
        for (index, (key, port)) in keys.into_iter().zip(&ports).enumerate() {
            let directory = root.join(format!("{generation}-{index}"));
            std::fs::create_dir_all(&directory).unwrap();
            JSONPeerSet::new(directory.to_str().unwrap(), true)
                .write(&peers)
                .unwrap();
            let provider = Arc::new(HashgraphConsensusProvider::for_node(
                &format!("{generation}-{index}"),
                &format!("{generation}-{index}"),
            ));
            let mut config = Config::new_default_config(&format!("127.0.0.1:{port}"));
            config.bind_addr = format!("127.0.0.1:{port}");
            config.set_data_dir(directory.to_str().unwrap());
            config.store = false;
            config.heartbeat_timeout = Duration::from_millis(20);
            config.moniker = format!("{generation}-{index}");
            config.proxy = Some(provider.proxy());
            config.key = Some(key);
            let mut engine = Babble::new(Arc::new(config));
            eprintln!("mesh {generation}: init validator {index}");
            engine.init(None, "main", "main", None).unwrap();
            providers.push(provider);
            engines.push(engine);
        }
        for engine in &engines {
            engine.node.as_ref().unwrap().run_async(true);
        }
        eprintln!("mesh {generation}: {PEERS} validators gossiping");
        Self { providers, engines }
    }

    fn port(&self, index: usize) -> &dyn ConsensusProvider {
        self.providers[index].as_ref()
    }

    /// Wait until every peer has finalized `count` records and none is pending.
    fn converge(&self, count: usize) -> Vec<Vec<Finalized>> {
        let started = Instant::now();
        loop {
            let views: Vec<Vec<Finalized>> = (0..PEERS)
                .map(|index| {
                    self.port(index)
                        .finalized_after(Epoch::GENESIS, 10_000)
                        .unwrap()
                })
                .collect();
            let pending: u64 = (0..PEERS)
                .map(|index| self.port(index).pending().unwrap())
                .sum();
            if pending == 0 && views.iter().all(|view| view.len() == count) {
                return views;
            }
            assert!(
                started.elapsed() < DEADLINE,
                "mesh did not converge on {count} records: {:?} finalized, {pending} pending",
                views.iter().map(Vec::len).collect::<Vec<_>>()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn shutdown(self) {
        for engine in &self.engines {
            if let Some(node) = &engine.node {
                node.shutdown();
            }
        }
    }
}

fn submit(mesh: &Mesh, peer: usize, journal: &str) {
    SubmitFinanceRecord {
        consensus: mesh.port(peer),
    }
    .execute(journal)
    .unwrap();
}

#[test]
fn a_live_mesh_hands_its_finalized_epoch_to_a_new_mesh_through_a_checkpoint() {
    let root = std::env::temp_dir().join(format!("aseman-hashgraph-mesh-{}", uuid::Uuid::new_v4()));
    let outgoing = Mesh::start(&root, "outgoing");

    // Records enter through different validators; the mesh orders all of them once.
    for record in 0..12 {
        submit(&outgoing, record % PEERS, &format!("journal-{record}"));
    }
    // A retried submission of an already-ordered record is idempotent.
    submit(&outgoing, 1, "journal-0");
    eprintln!("outgoing: 13 submissions accepted");
    let views = outgoing.converge(12);
    eprintln!("outgoing: 12 records finalized identically on {PEERS} peers");
    for view in &views[1..] {
        assert_eq!(view, &views[0], "peers disagree on the finalized order");
    }
    assert!(
        views[0]
            .iter()
            .any(|item| item.digest == journal_digest("journal-7"))
    );

    // Handover readiness: nothing in flight, and every peer checkpoints the same
    // order at the same finalized epoch.
    let outgoing_epoch = FinanceConsensusStatus {
        consensus: outgoing.port(0),
    }
    .finalized_epoch()
    .unwrap();
    eprintln!("outgoing: finalized epoch {outgoing_epoch:?}");
    let readiness = CheckHandoverReadiness {
        consensus: outgoing.port(0),
    };
    let ready = readiness.execute(1_000).unwrap();
    eprintln!("outgoing: readiness {ready:?}");
    readiness.may_switch(&ready, outgoing_epoch).unwrap();
    assert_eq!(ready.pending, 0);
    assert_eq!(ready.checkpoint.record_count, 12);
    for index in 1..PEERS {
        let other = outgoing.port(index).checkpoint(1_000).unwrap();
        assert_eq!(other.digest, ready.checkpoint.digest);
        assert_eq!(other.epoch, ready.checkpoint.epoch);
    }
    // A switch is refused at any epoch other than the verified one.
    assert!(
        readiness
            .may_switch(&ready, outgoing_epoch.next().unwrap())
            .is_err()
    );

    eprintln!("outgoing: all peers checkpoint the same order");

    // The outgoing mesh stops ordering before the incoming one starts: a switch never
    // has two providers ordering the same ledger. Its providers keep their history.
    let outgoing_providers = outgoing.providers.clone();
    outgoing.shutdown();
    eprintln!("outgoing: mesh stopped");
    // The incoming mesh adopts the checkpoint on every validator before ordering.
    let incoming = Mesh::start(&root, "incoming");
    for index in 0..PEERS {
        AdoptConsensusCheckpoint {
            consensus: incoming.port(index),
        }
        .execute(&ready.checkpoint)
        .unwrap();
    }
    for record in 12..18 {
        submit(&incoming, record % PEERS, &format!("journal-{record}"));
    }
    eprintln!("incoming: checkpoint adopted by {PEERS} validators; 6 submissions accepted");
    let continued = incoming.converge(6);
    eprintln!("incoming: 6 records finalized identically on top of the checkpoint");
    for view in &continued[1..] {
        assert_eq!(view, &continued[0]);
    }
    let incoming_checkpoint = incoming.port(0).checkpoint(2_000).unwrap();
    assert_eq!(incoming_checkpoint.record_count, 18);
    for index in 1..PEERS {
        assert_eq!(
            incoming.port(index).checkpoint(2_000).unwrap().digest,
            incoming_checkpoint.digest
        );
    }
    // A provider with history of its own can never adopt: that would fork the order.
    assert!(
        AdoptConsensusCheckpoint {
            consensus: incoming.port(0),
        }
        .execute(&ready.checkpoint)
        .is_err()
    );

    // Rollback: the outgoing providers still hold the prior order unchanged, and a
    // fresh provider restores it from the same checkpoint.
    assert_eq!(
        outgoing_providers[0].checkpoint(1_000).unwrap().digest,
        ready.checkpoint.digest
    );
    let restored = HashgraphConsensusProvider::new();
    AdoptConsensusCheckpoint {
        consensus: &restored,
    }
    .execute(&ready.checkpoint)
    .unwrap();
    assert_eq!(restored.checkpoint(3_000).unwrap().record_count, 12);

    incoming.shutdown();
    let _ = std::fs::remove_dir_all(root);
}
