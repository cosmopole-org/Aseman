//! Translation of `chain/proxy/handlers.go`.

use anyhow::Result;

use super::types::CommitResponse;
use crate::hashgraph::Block;
use crate::node::state::State;

/// Callbacks called by the in-memory proxy — the true contact surface between
/// Babble and the application.
pub trait ProxyHandler: Send + Sync {
    /// Called when Babble commits a block to the application.
    fn commit_handler(&self, block: Block) -> Result<CommitResponse>;
    /// Called by Babble to retrieve a snapshot for a block.
    fn snapshot_handler(&self, block_index: i64) -> Result<Vec<u8>>;
    /// Called by Babble to restore the application to a state.
    fn restore_handler(&self, snapshot: &[u8]) -> Result<Vec<u8>>;
    /// Called to notify that a Babble node entered a certain state.
    fn state_change_handler(&self, state: State) -> Result<()>;
}
