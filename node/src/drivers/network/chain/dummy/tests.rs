//! Translation of `chain/dummy/inmem_dummy_test.go`.
//!
//! `TestInmemDummyAppSide` / `TestInmemDummyServerSide` are reproduced; the
//! socket dummy tests are dropped along with the socket proxies.

use std::thread;
use std::time::Duration;

use crate::compat::logrus::Entry;
use crate::drivers::network::chain::crypto;
use crate::drivers::network::chain::hashgraph::{Block, InternalTransaction, TransactionType};
use crate::drivers::network::chain::node::state::State as NodeState;
use crate::drivers::network::chain::peers::Peer;
use crate::drivers::network::chain::proxy::AppProxy;

use super::InmemDummyClient;

// Translation of `inmem_dummy_test.go::TestInmemDummyAppSide`.
#[test]
fn inmem_dummy_app_side() {
    let dummy = InmemDummyClient::new(Entry::standalone());
    let submit_rx = dummy.proxy.submit_ch();
    let tx = b"the test transaction".to_vec();
    let tx_clone = tx.clone();

    let handle = thread::spawn(move || {
        let received = submit_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("submit timeout");
        assert_eq!(received, tx_clone);
    });

    dummy.proxy.submit_tx(&tx).unwrap();
    handle.join().unwrap();
}

// Translation of `inmem_dummy_test.go::TestInmemDummyServerSide`.
#[test]
fn inmem_dummy_server_side() {
    let dummy = InmemDummyClient::new(Entry::standalone());

    let blocks: Vec<Block> = (0..5)
        .map(|i| {
            Block::new(
                i,
                i + 1,
                Vec::new(),
                Vec::new(),
                vec![format!("block {} transaction", i).into_bytes()],
                vec![
                    InternalTransaction::new(
                        TransactionType::PeerAdd,
                        Peer::new("node0", "paris", ""),
                    ),
                    InternalTransaction::new(
                        TransactionType::PeerRemove,
                        Peer::new("node1", "london", ""),
                    ),
                ],
                0,
            )
        })
        .collect();

    // Commit the first block and verify the state hash.
    let resp = dummy
        .proxy
        .commit_block(blocks[0].clone())
        .expect("commit block 0");

    let mut expected_state_hash: Vec<u8> = Vec::new();
    for tx in blocks[0].transactions() {
        let tx_hash = crypto::sha256(tx);
        expected_state_hash = crypto::simple_hash_from_two_hashes(&expected_state_hash, &tx_hash);
    }
    assert_eq!(resp.state_hash, expected_state_hash);

    let snapshot = dummy
        .proxy
        .get_snapshot(blocks[0].index())
        .expect("snapshot");
    assert_eq!(snapshot, expected_state_hash);

    // Commit the rest, then restore to block 0.
    for b in blocks.iter().skip(1) {
        dummy.proxy.commit_block(b.clone()).expect("commit");
    }

    dummy.proxy.restore(&snapshot).expect("restore");

    // The restored state hash should equal block 0's.
    let resp = dummy
        .proxy
        .commit_block(blocks[0].clone())
        .expect("recommit block 0 after restore");
    // After restore + recommit, the state hash should fold block 0's
    // transactions onto the restored hash.
    let mut hash = expected_state_hash.clone();
    for tx in blocks[0].transactions() {
        hash = crypto::simple_hash_from_two_hashes(&hash, &crypto::sha256(tx));
    }
    assert_eq!(resp.state_hash, hash);

    dummy
        .proxy
        .on_state_changed(NodeState::Babbling)
        .expect("state change");
}

// End-to-end style test: a full "node round" of signed peer-add internal
// transactions are submitted through the proxy, committed via blocks, and the
// resulting state hash is deterministic and reproducible.
#[test]
fn dummy_chain_signed_internal_transactions_round_trip_state_hash() {
    use crate::drivers::network::chain::crypto::keys::{generate_ecdsa_key, public_key_hex};

    let dummy = InmemDummyClient::new(Entry::standalone());

    // 3 validator keys; each will issue a signed PeerAdd internal transaction.
    let keys: Vec<_> = (0..3).map(|_| generate_ecdsa_key().unwrap()).collect();
    let peers: Vec<Peer> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| {
            Peer::new(
                &public_key_hex(k.verifying_key()),
                &format!("addr{}", i),
                &format!("validator-{}", i),
            )
        })
        .collect();

    let mut itxs: Vec<InternalTransaction> = peers
        .iter()
        .map(|p| InternalTransaction::new(TransactionType::PeerAdd, p.clone()))
        .collect();
    for (itx, key) in itxs.iter_mut().zip(keys.iter()) {
        itx.sign(key).expect("sign internal transaction");
        assert!(itx.verify().expect("verify"), "self-signed itx must verify");
    }

    // Build two blocks: one contains application transactions, the other
    // contains the signed internal transactions.
    let app_block = Block::new(
        0,
        1,
        Vec::new(),
        Vec::new(),
        vec![b"app-tx-1".to_vec(), b"app-tx-2".to_vec()],
        Vec::new(),
        0,
    );
    let itx_block = Block::new(1, 2, Vec::new(), Vec::new(), Vec::new(), itxs.clone(), 0);

    let resp_app = dummy
        .proxy
        .commit_block(app_block.clone())
        .expect("commit app");
    let resp_itx = dummy
        .proxy
        .commit_block(itx_block.clone())
        .expect("commit itx");

    // The internal-transaction block produces a receipt for every itx, all
    // accepted by the dummy state.
    assert_eq!(resp_itx.internal_transaction_receipts.len(), itxs.len());
    assert!(resp_itx
        .internal_transaction_receipts
        .iter()
        .all(|r| r.accepted));

    // The committed-transactions buffer holds the application transactions in
    // commit order, untouched by the internal-transaction block.
    let committed = dummy.get_committed_transactions();
    assert_eq!(committed, vec![b"app-tx-1".to_vec(), b"app-tx-2".to_vec()]);

    // Replaying the same blocks against a fresh dummy must yield identical
    // state hashes — proof that commit is deterministic.
    let dummy2 = InmemDummyClient::new(Entry::standalone());
    let resp_app_2 = dummy2.proxy.commit_block(app_block).expect("re-commit app");
    let resp_itx_2 = dummy2.proxy.commit_block(itx_block).expect("re-commit itx");
    assert_eq!(resp_app.state_hash, resp_app_2.state_hash);
    assert_eq!(resp_itx.state_hash, resp_itx_2.state_hash);

    // Sanity: the running hash must have changed between blocks (app block had
    // transactions). The internal-transaction block had no application txs and
    // therefore doesn't move the hash, exercising the "no-tx commit" path.
    assert_ne!(resp_app.state_hash, Vec::<u8>::new());
    assert_eq!(resp_app.state_hash, resp_itx.state_hash);

    // Snapshot/restore must round-trip across an empty handler.
    let snap = dummy.proxy.get_snapshot(0).expect("snapshot");
    assert_eq!(snap, resp_app.state_hash);
    dummy.proxy.restore(&snap).expect("restore");
}

// Submitting the same transaction many times keeps order, and submissions are
// delivered exactly once each (no duplication / loss).
#[test]
fn dummy_proxy_submit_channel_preserves_order_and_count() {
    let dummy = InmemDummyClient::new(Entry::standalone());
    let rx = dummy.proxy.submit_ch();

    for i in 0..50 {
        dummy
            .proxy
            .submit_tx(format!("tx-{:02}", i).as_bytes())
            .unwrap();
    }

    let mut received: Vec<Vec<u8>> = Vec::new();
    for _ in 0..50 {
        received.push(rx.recv_timeout(Duration::from_millis(200)).expect("recv"));
    }

    assert_eq!(received.len(), 50);
    for (i, tx) in received.iter().enumerate() {
        assert_eq!(tx, format!("tx-{:02}", i).as_bytes());
    }
    // No further messages remain on the channel.
    assert!(rx.try_recv().is_err());
}
