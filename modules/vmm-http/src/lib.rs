//! The A501 node-to-VMM transport (plan 04, ADR 0029): the mutual-TLS server over
//! the VMM service use cases, and the node's blocking client.
#![forbid(unsafe_code)]

pub mod client;
pub mod server;
pub mod wire;
