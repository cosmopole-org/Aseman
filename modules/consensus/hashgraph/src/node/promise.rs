//! Translation of `chain/node/promise.go`.

use crossbeam_channel::{Receiver, Sender, bounded};

use crate::hashgraph::InternalTransaction;
use crate::peers::Peer;

/// The struct returned by a [`JoinPromise`].
#[derive(Debug, Clone, Default)]
pub struct JoinPromiseResponse {
    pub accepted: bool,
    pub accepted_round: i64,
    pub peers: Vec<Peer>,
}

/// A relay between the join-request RPC handler and the consensus system which
/// asynchronously processes the corresponding `InternalTransaction`. Also used
/// with leave requests.
pub struct JoinPromise {
    pub tx: InternalTransaction,
    pub resp_ch: Sender<JoinPromiseResponse>,
    pub resp_rx: Receiver<JoinPromiseResponse>,
}

impl JoinPromise {
    /// Creates a new `JoinPromise`.
    pub fn new(tx: InternalTransaction) -> JoinPromise {
        // Buffered so responding never blocks if there is no listener.
        let (resp_ch, resp_rx) = bounded(2);
        JoinPromise {
            tx,
            resp_ch,
            resp_rx,
        }
    }

    /// Sends a `JoinPromiseResponse` to the promise.
    pub fn respond(&self, accepted: bool, accepted_round: i64, peers: Vec<Peer>) {
        let _ = self.resp_ch.send(JoinPromiseResponse {
            accepted,
            accepted_round,
            peers,
        });
    }
}
