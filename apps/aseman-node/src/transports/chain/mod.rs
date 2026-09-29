//! The main chain: Babble shards ordering the node's replicated requests and
//! messages, with the Hashgraph consensus provider as the main chain's
//! application handler (governance and finance ordering).

pub mod blockchain;
pub(crate) mod callbacks;
pub(crate) mod globe;
mod shard_bootstrap;

pub use blockchain::{Blockchain, ChainSettings};
