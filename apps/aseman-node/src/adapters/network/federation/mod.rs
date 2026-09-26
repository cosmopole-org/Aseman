//! Inter-organisation TCP RPC — the federation transport the Caspar shell
//! uses to talk to peer nodes (sister Caspar deployments) and federate
//! updates / requests / responses across them.
//!
//! - `netserver` — framed TCP server (`Tcp` / `Socket`).
//! - `fednet` — the `IFederation` implementation wired on top.

pub mod fednet;
pub mod netserver;

pub use fednet::FedNet;
pub use netserver::{Socket, Tcp};
