//! Callbacks the chain transport completes: a base request's response and the
//! pipeline committed blocks flow through.

use std::sync::Arc;

/// A chain callback invoked when a transaction settles.
pub type ChainCallbackFn = Arc<dyn Fn(Vec<u8>, i64, Option<anyhow::Error>) + Send + Sync>;

/// Callback awaiting responses from a set of chain executors.
#[derive(Clone)]
pub struct ChainCallback {
    pub fn_: ChainCallbackFn,
}

/// Callback receiving a chain base-request response.
pub type BaseResponseCallback = Box<dyn Fn(Vec<u8>, i64, Option<anyhow::Error>) + Send + Sync>;

/// Pipeline callback. Receives a batch of payloads and a per-payload
/// emit callback; returns the keys of the messages that were forwarded.
pub type PipelineFn =
    Box<dyn Fn(Vec<Vec<u8>>, Box<dyn Fn(Vec<u8>) + Send + Sync>) -> Vec<String> + Send + Sync>;
