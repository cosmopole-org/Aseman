//! Shared types and helpers for the core `Globe` compatibility coordinator.
//!
//! Translation of `core/module/globe/globe.go`: the packet envelope and the
//! injected closure signatures. Staking and election logic — including the
//! action constants and the election-meta builder — lives in the consensus
//! engine's `governance` subsystem (RL-011); the globe keeps only chain
//! transport.

use std::sync::Arc;

use crate::models::chain::{
    ChainBaseRequest, ChainCallback, ChainMessage, ChainResponse, MessageCallback,
};

/// Signature for `signPacketFn` injected at construction time.
pub type SignPacketFn = Arc<dyn Fn(&[u8]) -> String + Send + Sync>;

/// Signature for `submitChainPacketFn` — payload boxed as `AnyVal` so the
/// caller can shuttle any of the chain packet variants.
pub type SubmitChainPacketFn = Arc<dyn Fn(&str, ChainPacketOp) + Send + Sync>;

/// Closure stash for chain base callbacks.
pub type SetChainCallbackFn = Arc<dyn Fn(&str, ChainCallback) + Send + Sync>;

/// Closure stash for typed-message callbacks.
pub type SetMessageCbFn = Arc<dyn Fn(&str, MessageCallback) + Send + Sync>;

/// All packet variants the submit-chain function can carry. Mirrors the Go
/// switch in `core.go::handleChainPacket`. This is the chain transport wire
/// envelope for request/response/message only; stake and election are owned
/// entirely by the consensus provider (RL-011).
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // legacy wire envelope
pub enum ChainPacketOp {
    BaseRequest(ChainBaseRequest),
    Message(ChainMessage),
    Response(ChainResponse),
}
