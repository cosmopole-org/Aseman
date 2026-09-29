//! The chain packet envelope and the closures the globe is built from.

use std::sync::Arc;

use crate::transports::chain::callbacks::ChainCallback;
use aseman_contracts::wire::chain::{ChainBaseRequest, ChainMessage};

/// Signature for `signPacketFn` injected at construction time.
pub type SignPacketFn = Arc<dyn Fn(&[u8]) -> String + Send + Sync>;

/// Signature for `submitChainPacketFn` — payload boxed as `AnyVal` so the
/// caller can shuttle any of the chain packet variants.
pub type SubmitChainPacketFn = Arc<dyn Fn(&str, ChainPacketOp) + Send + Sync>;

/// Closure stash for chain base callbacks.
pub type SetChainCallbackFn = Arc<dyn Fn(&str, ChainCallback) + Send + Sync>;

/// All packet variants the submit-chain function can carry. One variant per
/// switch in `core.go::handleChainPacket`. This is the chain transport wire
/// envelope for request/response/message only; stake and election are owned
/// entirely by the consensus provider.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // wire envelope
pub enum ChainPacketOp {
    BaseRequest(ChainBaseRequest),
    Message(ChainMessage),
}
