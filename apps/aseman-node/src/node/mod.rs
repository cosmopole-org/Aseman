//! The running node: its identity, prices, and components, and how it starts
//! them ([`Node::load`]).
//!
//! - [`types`] — the node and its components.
//! - [`constructor`] — building a node from its configuration.
//! - [`load`] — starting its components.
//! - [`accessors`] — its accessors and transactions.
//! - [`chain`] — the packets the main chain commits.
//! - [`crypto`] — its RSA keys and signatures.
//! - [`finance`] — its prices and free nodes.

mod accessors;
mod chain;
mod constructor;
mod crypto;
pub(crate) mod finance;
mod load;
#[cfg(test)]
mod testing;
mod types;

pub use types::{Node, Tools};
