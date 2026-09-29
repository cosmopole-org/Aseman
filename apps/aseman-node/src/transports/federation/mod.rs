//! Node-to-node federation over framed TLS TCP: requests forwarded to the node
//! that owns their origin, and store updates pushed to peer nodes holding members.
//!
//! - `netserver` — the framed TCP server and sockets.
//! - `fednet` — requests, responses, and updates over it.

pub mod fednet;
pub mod netserver;

pub use fednet::FedNet;

/// Callback delivering a federation response — payload, status code, error.
pub type FedRequestCallback = Box<dyn Fn(Vec<u8>, i64, Option<anyhow::Error>) + Send + Sync>;
