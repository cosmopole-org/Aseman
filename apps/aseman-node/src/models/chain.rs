//! Chain wire DTOs migrated to `aseman_contracts::legacy_wire::chain` (RL-002),
//! kept as a compatibility re-export shim; the in-memory orchestration types
//! (`ChainCallback`, `MessageCallback`, `ChainCallbackFn`) stay here until the
//! legacy consensus path retires (RL-011).

pub use aseman_contracts::legacy_wire::chain::*;

use std::collections::HashMap;
use std::sync::Arc;

use crate::legacy::utils::compat::GoError;

/// A chain callback invoked when a transaction settles.
pub type ChainCallbackFn = Arc<dyn Fn(Vec<u8>, i64, Option<GoError>) + Send + Sync>;

/// Callback awaiting responses from a set of chain executors.
#[derive(Clone)]
pub struct ChainCallback {
    pub fn_: ChainCallbackFn,
    pub executors: HashMap<String, bool>,
    pub responses: HashMap<String, String>,
    pub tag: String,
}

/// Callback awaiting a single typed chain message reply.
#[derive(Clone)]
pub struct MessageCallback {
    pub id: String,
    pub fn_: Arc<dyn Fn(String, Vec<u8>) + Send + Sync>,
}
