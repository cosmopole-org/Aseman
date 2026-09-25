//! Hashgraph/Babble consensus engine extracted from the canonical node (RL-011).
//!
//! This crate owns the algorithm, peer transport, event store, and consensus-node
//! machinery. Node-specific finance/action translation remains in the node adapter.
#![forbid(unsafe_code)]

pub mod babble;
pub mod common;
pub mod config;
pub mod crypto;
pub mod dummy;
pub mod hashgraph;
pub mod logrus;
pub mod net;
pub mod node;
pub mod peers;
pub mod provider;
pub mod proxy;
pub mod util;
