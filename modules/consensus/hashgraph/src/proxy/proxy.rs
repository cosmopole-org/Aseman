//! Translation of `chain/proxy/proxy.go`.

use anyhow::Result;
use crossbeam_channel::Receiver;

use super::types::CommitResponse;
use crate::hashgraph::Block;
use crate::node::state::State;

/// The interface Babble uses to communicate with the application.
pub trait AppProxy: Send + Sync {
    /// A channel on which the application submits transactions.
    fn submit_ch(&self) -> Receiver<Vec<u8>>;
    /// Commits a block to the application.
    fn commit_block(&self, block: Block) -> Result<CommitResponse>;
    /// Retrieves the application snapshot for a block.
    fn get_snapshot(&self, block_index: i64) -> Result<Vec<u8>>;
    /// Restores the application to a snapshot.
    fn restore(&self, snapshot: &[u8]) -> Result<()>;
    /// Notifies the application that the node entered a given state.
    fn on_state_changed(&self, state: State) -> Result<()>;
}
