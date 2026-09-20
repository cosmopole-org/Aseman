//! Translation of `chain/proxy/inmem/inmem_proxy.go`.
//!
//! The `InmemProxy` is the in-memory `AppProxy`: the application drops a
//! `ProxyHandler` into it and Babble can be embedded directly into the host
//! binary. Transactions are submitted through a `crossbeam_channel`, replacing
//! Go's `chan []byte`.

use std::sync::Arc;

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::compat::logrus::Entry;
use crate::drivers::network::chain::hashgraph::Block;
use crate::drivers::network::chain::node::state::State;
use crate::drivers::network::chain::proxy::{AppProxy, CommitResponse, ProxyHandler};

/// In-memory `AppProxy` implementation that forwards every callback to a
/// user-supplied [`ProxyHandler`].
pub struct InmemProxy {
    handler: Arc<dyn ProxyHandler>,
    submit_tx: Sender<Vec<u8>>,
    submit_rx: Receiver<Vec<u8>>,
    logger: Entry,
}

impl InmemProxy {
    /// Instantiates an `InmemProxy` from a handler.
    pub fn new(handler: Arc<dyn ProxyHandler>, logger: Option<Entry>) -> Self {
        let (submit_tx, submit_rx) = unbounded();
        InmemProxy {
            handler,
            submit_tx,
            submit_rx,
            logger: logger.unwrap_or_else(Entry::standalone),
        }
    }

    /// Called by the App to submit a transaction to Babble.
    ///
    /// As in Go, the bytes are copied so the application is free to recycle
    /// the buffer once `submit_tx` returns.
    pub fn submit_tx(&self, tx: &[u8]) -> Result<()> {
        let owned = tx.to_vec();
        self.submit_tx
            .send(owned)
            .map_err(|e| anyhow::anyhow!("submit_tx: {}", e))
    }
}

impl AppProxy for InmemProxy {
    fn submit_ch(&self) -> Receiver<Vec<u8>> {
        self.submit_rx.clone()
    }

    fn commit_block(&self, block: Block) -> Result<CommitResponse> {
        let res = self.handler.commit_handler(block);
        if let Err(e) = &res {
            self.logger.with_error(e).debug("InmemProxy.CommitBlock");
        }
        res
    }

    fn get_snapshot(&self, block_index: i64) -> Result<Vec<u8>> {
        let res = self.handler.snapshot_handler(block_index);
        self.logger
            .with_field("block", block_index)
            .debug("InmemProxy.GetSnapshot");
        res
    }

    fn restore(&self, snapshot: &[u8]) -> Result<()> {
        let res = self.handler.restore_handler(snapshot);
        self.logger.debug("InmemProxy.Restore");
        res.map(|_| ())
    }

    fn on_state_changed(&self, state: State) -> Result<()> {
        self.handler.state_change_handler(state)
    }
}
