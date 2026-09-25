//! Translation of `chain/proxy/types.go`.

use anyhow::Result;

use crate::hashgraph::{Block, InternalTransactionReceipt};

/// The result of committing a block to the application.
#[derive(Debug, Clone, Default)]
pub struct CommitResponse {
    pub state_hash: Vec<u8>,
    pub internal_transaction_receipts: Vec<InternalTransactionReceipt>,
}

/// A callback invoked to commit a block.
pub type CommitCallback = Box<dyn Fn(Block) -> Result<CommitResponse> + Send + Sync>;

/// A commit callback used for testing — accepts every internal transaction.
pub fn dummy_commit_callback(block: Block) -> Result<CommitResponse> {
    let receipts: Vec<InternalTransactionReceipt> = block
        .internal_transactions()
        .iter()
        .map(|it| it.as_accepted())
        .collect();
    Ok(CommitResponse {
        state_hash: Vec::new(),
        internal_transaction_receipts: receipts,
    })
}
