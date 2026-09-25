//! Translation of `chain/dummy/state.go`.
//!
//! `State` is the toy application Babble's tests run against. It implements
//! [`ProxyHandler`] so an `InmemProxy` can be wired straight into it. Block
//! transactions are appended in-order and folded into a running hash; snapshots
//! correspond to the state hash after each committed block.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Result, anyhow};

use crate::crypto;
use crate::hashgraph::{Block, InternalTransactionReceipt};
use crate::logrus::Entry;
use crate::node::state::State as NodeState;
use crate::proxy::{CommitResponse, ProxyHandler};

/// Toy application implementing [`ProxyHandler`].
pub struct State {
    inner: Mutex<Inner>,
    logger: Entry,
}

struct Inner {
    committed_txs: Vec<Vec<u8>>,
    state_hash: Vec<u8>,
    snapshots: HashMap<i64, Vec<u8>>,
    babble_state: NodeState,
}

impl State {
    /// Instantiate a new dummy state.
    pub fn new(logger: Entry) -> Self {
        logger.info("Init Dummy State");
        State {
            inner: Mutex::new(Inner {
                committed_txs: Vec::new(),
                state_hash: Vec::new(),
                snapshots: HashMap::new(),
                babble_state: NodeState::default(),
            }),
            logger,
        }
    }

    /// Returns the list of committed transactions (clone).
    pub fn get_committed_transactions(&self) -> Vec<Vec<u8>> {
        self.inner.lock().unwrap().committed_txs.clone()
    }
}

impl ProxyHandler for State {
    fn commit_handler(&self, block: Block) -> Result<CommitResponse> {
        let mut inner = self.inner.lock().unwrap();

        // Block transactions are ordered — every node will receive the same
        // transactions in the same order.
        let txs: Vec<Vec<u8>> = block.transactions().to_vec();
        inner.committed_txs.extend(txs.iter().cloned());

        // Running cumulative hash of every transaction.
        let mut hash = inner.state_hash.clone();
        for tx in &txs {
            // Match Go's `a.logger.Info(string(tx))`.
            self.logger.info(String::from_utf8_lossy(tx));
            hash = crypto::simple_hash_from_two_hashes(&hash, &crypto::sha256(tx));
        }
        inner.state_hash = hash.clone();
        inner.snapshots.insert(block.index(), hash);

        // Accept every internal transaction. A real application would gate
        // these on its peer-set policy.
        let receipts: Vec<InternalTransactionReceipt> = block
            .internal_transactions()
            .iter()
            .map(|it| it.as_accepted())
            .collect();

        Ok(CommitResponse {
            state_hash: inner.state_hash.clone(),
            internal_transaction_receipts: receipts,
        })
    }

    fn snapshot_handler(&self, block_index: i64) -> Result<Vec<u8>> {
        let inner = self.inner.lock().unwrap();
        self.logger
            .with_field("block", block_index)
            .debug("GetSnapshot");
        inner
            .snapshots
            .get(&block_index)
            .cloned()
            .ok_or_else(|| anyhow!("Snapshot {} not found", block_index))
    }

    fn restore_handler(&self, snapshot: &[u8]) -> Result<Vec<u8>> {
        let mut inner = self.inner.lock().unwrap();
        inner.state_hash = snapshot.to_vec();
        Ok(inner.state_hash.clone())
    }

    fn state_change_handler(&self, state: NodeState) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.babble_state = state;
        self.logger
            .with_field("state", state)
            .debug("StateChangeHandler");
        Ok(())
    }
}
