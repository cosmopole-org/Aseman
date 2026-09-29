//! The chain request/response plumbing: submitting a base request and
//! completing its callback when the chain runs it. Staking and the validator
//! election belong to the consensus provider's governance.
//!
//! - [`types`] — the packet envelope and injected closure signatures.
//! - [`transport`] — sending base requests.

mod transport;
mod types;

pub use types::{ChainPacketOp, SetChainCallbackFn, SignPacketFn, SubmitChainPacketFn};

use std::sync::Arc;

/// `Globe` is the chain-RPC transport coordinator.
pub struct Globe {
    node_id: String,
    sign_packet_fn: SignPacketFn,
    submit_chain_packet_fn: SubmitChainPacketFn,
    set_chain_callback_fn: SetChainCallbackFn,
}

impl Globe {
    /// Mirrors `NewGlobe(...)`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_id: String,
        sign_packet_fn: SignPacketFn,
        submit_chain_packet_fn: SubmitChainPacketFn,
        set_chain_callback_fn: SetChainCallbackFn,
    ) -> Arc<Globe> {
        Arc::new(Globe {
            node_id,
            sign_packet_fn,
            submit_chain_packet_fn,
            set_chain_callback_fn,
        })
    }
}
