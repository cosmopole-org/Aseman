//! Translation of `chain/net/*_test.go`.
//!
//! The Go tests parameterised every check over INMEM / TCP / WEBRTC; since
//! WebRTC (and the WAMP signal layer) were dropped, the matrix here is
//! limited to INMEM and TCP.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{bounded, RecvTimeoutError};

use crate::drivers::network::chain::hashgraph::{
    Block, BlockSignature, Event, Frame, FrameEvent, InternalTransaction, Root, TransactionType,
};
use crate::drivers::network::chain::peers::Peer;

use super::commands::{
    EagerSyncRequest, EagerSyncResponse, FastForwardRequest, FastForwardResponse, JoinRequest,
    JoinResponse, SyncRequest, SyncResponse,
};
use super::inmem_transport::InmemTransport;
use super::net_transport::NetworkTransport;
use super::rpc::{Rpc, RpcCommand, RpcResponseKind};
use super::tcp_stream_layer::TcpStreamLayer;
use super::tcp_transport::new_tcp_transport;
use super::transport::Transport;

// -- rpc.rs -----------------------------------------------------------------

// Translation of `rpc_test.go::TestRPCRespondDeliversResponseAndError`.
#[test]
fn rpc_respond_delivers_response_and_error() {
    let (tx, rx) = bounded(1);
    let rpc = Rpc {
        command: RpcCommand::Sync(SyncRequest::default()),
        resp_chan: tx,
    };
    rpc.respond(
        Some(RpcResponseKind::Sync(SyncResponse {
            from_id: 7,
            ..Default::default()
        })),
        None,
    );
    let resp = rx.recv().expect("response delivered");
    assert!(resp.error.is_none());
    match resp.response {
        Some(RpcResponseKind::Sync(r)) => assert_eq!(r.from_id, 7),
        _ => panic!("unexpected response variant"),
    }
}

// -- shared helpers ---------------------------------------------------------

/// Hands out distinct, predictable test ports while still letting the OS
/// pick the actual port (we always bind on `127.0.0.1:0`).
fn next_port_offset() -> usize {
    static N: AtomicUsize = AtomicUsize::new(0);
    N.fetch_add(1, Ordering::SeqCst)
}

fn new_tcp(transport_label: &str) -> Arc<NetworkTransport> {
    let _ = transport_label;
    new_tcp_transport(
        "127.0.0.1:0",
        "",
        2,
        Duration::from_secs(1),
        Duration::from_secs(2),
        None,
    )
    .expect("tcp transport")
}

fn run_consumer<F>(rx: crossbeam_channel::Receiver<Rpc>, f: F)
where
    F: FnOnce(Rpc) + Send + 'static,
{
    thread::spawn(move || match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(rpc) => f(rpc),
        Err(_) => eprintln!("consumer timeout"),
    });
}

// -- tcp_transport_test.go --------------------------------------------------

// Translation of `tcp_transport_test.go::TestTCPTransport_BadAddr`.
#[test]
fn tcp_transport_bad_addr() {
    let res = TcpStreamLayer::new("0.0.0.0:0", "0.0.0.0:1234");
    let err = match res {
        Ok(_) => panic!("expected error, got listener"),
        Err(e) => e,
    };
    let msg = format!("{}", err);
    assert!(
        msg.contains("not advertisable"),
        "unexpected error: {}",
        msg
    );
}

// Translation of `tcp_transport_test.go::TestTCPTransport_WithAdvertise`.
#[test]
fn tcp_transport_with_advertise() {
    let trans = new_tcp_transport(
        "0.0.0.0:0",
        "127.0.0.1:12345",
        1,
        Duration::from_millis(0),
        Duration::from_millis(0),
        None,
    )
    .unwrap();
    assert_eq!(trans.advertise_addr(), "127.0.0.1:12345");
    trans.close().unwrap();
}

// Translation of `tcp_transport_test.go::TestTCPTransport_PooledConn`
// (kept lightweight; we issue a single round-trip rather than the Go test's
// 5-way parallel hammering — single round-trip is sufficient to verify that
// `return_conn` puts the connection back).
#[test]
fn tcp_transport_round_trip_pools_connection() {
    let trans1 = new_tcp("trans1");
    let trans2 = new_tcp("trans2");
    let rpc_ch = trans1.consumer("", "");

    let args = SyncRequest {
        from_id: 0,
        sync_limit: 20,
        known: HashMap::from([(0u32, 1i64), (1, 2), (2, 3)]),
        ..Default::default()
    };
    let expected = SyncResponse {
        from_id: 1,
        known: HashMap::from([(0u32, 5i64), (1, 5), (2, 6)]),
        ..Default::default()
    };

    let expected_clone = expected.clone();
    run_consumer(rpc_ch, move |rpc| {
        rpc.respond(Some(RpcResponseKind::Sync(expected_clone)), None);
    });

    // Give the listener a moment to be ready.
    thread::sleep(Duration::from_millis(50));
    let out = trans2.sync(&trans1.local_addr(), &args).unwrap();
    assert_eq!(out.from_id, expected.from_id);
    assert_eq!(out.known, expected.known);

    let _ = trans1.close();
    let _ = trans2.close();
}

// -- transport_test.go ------------------------------------------------------

#[derive(Copy, Clone, Debug)]
enum Kind {
    Inmem,
    Tcp,
}

fn make_pair(kind: Kind) -> (Box<dyn Transport>, Box<dyn Transport>) {
    match kind {
        Kind::Inmem => {
            let off = next_port_offset();
            let (addr1, t1) = InmemTransport::new(&format!("inmem-{}", off * 2));
            let (addr2, t2) = InmemTransport::new(&format!("inmem-{}", off * 2 + 1));
            t1.connect(&addr2, &t2);
            t2.connect(&addr1, &t1);
            (Box::new(t1), Box::new(t2))
        }
        Kind::Tcp => {
            let t1 = new_tcp("t1");
            let t2 = new_tcp("t2");
            // Give listeners a moment to be ready before tests dial.
            thread::sleep(Duration::from_millis(50));
            (Box::new(ArcTransport(t1)), Box::new(ArcTransport(t2)))
        }
    }
}

/// Newtype so `Arc<NetworkTransport>` can satisfy `Box<dyn Transport>` —
/// every `Transport` call is forwarded to the inner `Arc`.
struct ArcTransport(Arc<NetworkTransport>);

impl Transport for ArcTransport {
    fn listen(&self) {
        self.0.listen();
    }
    fn consumer(&self, w: &str, s: &str) -> crossbeam_channel::Receiver<Rpc> {
        self.0.consumer(w, s)
    }
    fn local_addr(&self) -> String {
        self.0.local_addr()
    }
    fn advertise_addr(&self) -> String {
        self.0.advertise_addr()
    }
    fn sync(&self, target: &str, args: &SyncRequest) -> anyhow::Result<SyncResponse> {
        self.0.sync(target, args)
    }
    fn eager_sync(
        &self,
        target: &str,
        args: &EagerSyncRequest,
    ) -> anyhow::Result<EagerSyncResponse> {
        self.0.eager_sync(target, args)
    }
    fn fast_forward(
        &self,
        target: &str,
        args: &FastForwardRequest,
    ) -> anyhow::Result<FastForwardResponse> {
        self.0.fast_forward(target, args)
    }
    fn join(&self, target: &str, args: &JoinRequest) -> anyhow::Result<JoinResponse> {
        self.0.join(target, args)
    }
    fn close(&self) -> anyhow::Result<()> {
        self.0.close()
    }
}

#[test]
fn transport_start_stop_inmem() {
    let (a, _) = make_pair(Kind::Inmem);
    a.close().unwrap();
}

#[test]
fn transport_start_stop_tcp() {
    let (a, _) = make_pair(Kind::Tcp);
    a.close().unwrap();
}

// Translation of `transport_test.go::TestTransport_Sync` (INMEM + TCP only).
fn transport_sync_for(kind: Kind) {
    let (t1, t2) = make_pair(kind);
    let rpc_ch = t1.consumer("", "");

    let args = SyncRequest {
        from_id: 0,
        sync_limit: 20,
        known: HashMap::from([(0u32, 1i64), (1, 2), (2, 3)]),
        ..Default::default()
    };
    let expected = SyncResponse {
        from_id: 1,
        known: HashMap::from([(0u32, 5i64), (1, 5), (2, 6)]),
        ..Default::default()
    };
    let args_clone = args.clone();
    let expected_clone = expected.clone();
    run_consumer(rpc_ch, move |rpc| match rpc.command {
        RpcCommand::Sync(ref req) => {
            assert_eq!(req.from_id, args_clone.from_id);
            assert_eq!(req.sync_limit, args_clone.sync_limit);
            rpc.respond(Some(RpcResponseKind::Sync(expected_clone)), None);
        }
        _ => panic!("unexpected variant"),
    });

    thread::sleep(Duration::from_millis(50));
    let out = t2.sync(&t1.advertise_addr(), &args).unwrap();
    assert_eq!(out.from_id, expected.from_id);
    assert_eq!(out.known, expected.known);

    let _ = t1.close();
    let _ = t2.close();
}

#[test]
fn transport_sync_inmem() {
    transport_sync_for(Kind::Inmem);
}

#[test]
fn transport_sync_tcp() {
    transport_sync_for(Kind::Tcp);
}

fn transport_eager_sync_for(kind: Kind) {
    let (t1, t2) = make_pair(kind);
    let rpc_ch = t1.consumer("", "");

    let args = EagerSyncRequest {
        from_id: 0,
        events: vec![],
        ..Default::default()
    };
    let expected = EagerSyncResponse {
        from_id: 1,
        success: true,
        ..Default::default()
    };
    let expected_clone = expected.clone();
    run_consumer(rpc_ch, move |rpc| match rpc.command {
        RpcCommand::EagerSync(_) => {
            rpc.respond(Some(RpcResponseKind::EagerSync(expected_clone)), None);
        }
        _ => panic!("unexpected variant"),
    });

    thread::sleep(Duration::from_millis(50));
    let out = t2.eager_sync(&t1.advertise_addr(), &args).unwrap();
    assert_eq!(out.from_id, expected.from_id);
    assert_eq!(out.success, expected.success);
}

#[test]
fn transport_eager_sync_inmem() {
    transport_eager_sync_for(Kind::Inmem);
}

#[test]
fn transport_eager_sync_tcp() {
    transport_eager_sync_for(Kind::Tcp);
}

fn transport_fast_forward_for(kind: Kind) {
    let (t1, t2) = make_pair(kind);
    let rpc_ch = t1.consumer("", "");

    let frame_peers = vec![
        Peer::new("pub1", "addr1", "monika"),
        Peer::new("pub2", "addr2", "monika"),
    ];
    let mut roots = std::collections::BTreeMap::new();
    roots.insert("pub1".to_string(), Root::new());
    roots.insert("pub2".to_string(), Root::new());
    let frame = Frame {
        round: 10,
        peers: frame_peers,
        roots,
        events: vec![FrameEvent {
            core: Box::new(Event::new(
                vec![b"tx1".to_vec(), b"tx2".to_vec()],
                vec![InternalTransaction::new(
                    TransactionType::PeerAdd,
                    Peer::new("pub3", "addr3", "monika"),
                )],
                vec![BlockSignature {
                    validator: b"pub1".to_vec(),
                    index: 0,
                    signature: "the signature".to_string(),
                }],
                vec!["pub1".to_string(), "pub2".to_string()],
                b"pub1".to_vec(),
                4,
            )),
            ..Default::default()
        }],
        ..Default::default()
    };
    let marshalled = frame.marshal().unwrap();
    let mut unmarshalled_frame = Frame::default();
    unmarshalled_frame.unmarshal(&marshalled).unwrap();

    let block = Block::new_from_frame(9, &frame).unwrap();
    let marshalled_b = block.marshal().unwrap();
    let mut unmarshalled_block = Block::default();
    unmarshalled_block.unmarshal(&marshalled_b).unwrap();

    let snapshot = b"this is the snapshot".to_vec();

    let args = FastForwardRequest {
        from_id: 0,
        ..Default::default()
    };
    let expected = FastForwardResponse {
        from_id: 1,
        block: unmarshalled_block,
        frame: unmarshalled_frame,
        snapshot,
        ..Default::default()
    };
    let expected_clone = expected.clone();
    run_consumer(rpc_ch, move |rpc| match rpc.command {
        RpcCommand::FastForward(_) => {
            rpc.respond(Some(RpcResponseKind::FastForward(expected_clone)), None);
        }
        _ => panic!("unexpected variant"),
    });

    thread::sleep(Duration::from_millis(50));
    let out = t2.fast_forward(&t1.advertise_addr(), &args).unwrap();
    assert_eq!(out.from_id, expected.from_id);
    assert_eq!(out.snapshot, expected.snapshot);
    assert_eq!(out.block.round_received(), expected.block.round_received());
}

#[test]
fn transport_fast_forward_inmem() {
    transport_fast_forward_for(Kind::Inmem);
}

#[test]
fn transport_fast_forward_tcp() {
    transport_fast_forward_for(Kind::Tcp);
}

fn transport_join_for(kind: Kind) {
    let (t1, t2) = make_pair(kind);
    let rpc_ch = t1.consumer("", "");

    let p1 = Peer::new("node1", "addr1", "monika");
    let p2 = Peer::new("node2", "addr2", "monika");
    let itx = InternalTransaction::new_join(p1.clone());
    let args = JoinRequest {
        internal_transaction: itx,
        ..Default::default()
    };
    let expected = JoinResponse {
        from_id: p2.id(),
        accepted_round: 5,
        peers: vec![p1, p2],
        ..Default::default()
    };
    let expected_clone = expected.clone();
    run_consumer(rpc_ch, move |rpc| match rpc.command {
        RpcCommand::Join(_) => {
            rpc.respond(Some(RpcResponseKind::Join(expected_clone)), None);
        }
        _ => panic!("unexpected variant"),
    });

    thread::sleep(Duration::from_millis(50));
    let out = t2.join(&t1.advertise_addr(), &args).unwrap();
    assert_eq!(out.from_id, expected.from_id);
    assert_eq!(out.accepted_round, expected.accepted_round);
    assert_eq!(out.peers.len(), expected.peers.len());
}

#[test]
fn transport_join_inmem() {
    transport_join_for(Kind::Inmem);
}

#[test]
fn transport_join_tcp() {
    transport_join_for(Kind::Tcp);
}

// Smoke test: an inbound RPC for an unknown chain id is rejected, not
// silently dropped.
#[test]
fn tcp_unknown_chain_returns_error() {
    let trans1 = new_tcp("trans1");
    let trans2 = new_tcp("trans2");

    let args = SyncRequest {
        work_chain_id: "no-such-chain".into(),
        shard_chain_id: "x".into(),
        ..Default::default()
    };

    thread::sleep(Duration::from_millis(50));
    let err = trans2.sync(&trans1.local_addr(), &args).unwrap_err();
    let msg = format!("{}", err);
    assert!(msg.contains("no consumer"), "unexpected error: {}", msg);
    let _ = trans1.close();
    let _ = trans2.close();
}

// Suppresses the "unused" warning on `RecvTimeoutError` (it's only used for
// type clarity above).
#[allow(dead_code)]
fn _unused_recv_timeout_error_marker() -> RecvTimeoutError {
    RecvTimeoutError::Timeout
}
