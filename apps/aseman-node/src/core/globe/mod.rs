//! Translation of `core/module/globe/globe.go`.
//!
//! `Globe` implements the [`IGlobe`] trait: chain request/response plumbing.
//! Validator-set staking and the hourly weighted-validator election live in the
//! consensus engine's `governance` subsystem (RL-011), driven by the provider's
//! autonomous scheduler and the orchestrator's chain-packet routing; the globe
//! is pure transport and carries no staking/election state or controllers.
//!
//! - [`types`] — the packet envelope and injected closure signatures.
//! - [`transport`] — the [`IGlobe`] trait surface (chain RPC).

#[cfg(test)]
mod tests;
mod transport;
mod types;

pub use types::{
    ChainPacketOp, SetChainCallbackFn, SetMessageCbFn, SignPacketFn, SubmitChainPacketFn,
};

use std::sync::Arc;

use crate::models::chain::ChainCallback;

/// `Globe` is the chain-RPC transport coordinator.
pub struct Globe {
    node_id: String,
    sign_packet_fn: SignPacketFn,
    submit_chain_packet_fn: SubmitChainPacketFn,
    set_chain_callback_fn: SetChainCallbackFn,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "RL-003: legacy orchestration surface kept until its deletion gate"
        )
    )]
    set_message_cb_fn: SetMessageCbFn,
}

impl Globe {
    /// Mirrors `NewGlobe(...)`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_id: String,
        sign_packet_fn: SignPacketFn,
        submit_chain_packet_fn: SubmitChainPacketFn,
        set_chain_callback_fn: SetChainCallbackFn,
        set_message_cb_fn: SetMessageCbFn,
    ) -> Arc<Globe> {
        Arc::new(Globe {
            node_id,
            sign_packet_fn,
            submit_chain_packet_fn,
            set_chain_callback_fn,
            set_message_cb_fn,
        })
    }
}

// Keep `ChainCallback` import referenced (used in callback stash typing).
const _: fn() -> Option<ChainCallback> = || None;
