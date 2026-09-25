//! Translation of `chain/net` — the transports Babble nodes use to exchange
//! RPC requests (SyncRequest, EagerSyncRequest, JoinRequest, ...).
//!
//! Phase 2.13 ships the transport-independent interface layer together with
//! two concrete transports:
//!
//! * `InmemTransport`  — in-process, used by tests.
//! * `NetworkTransport` over `TcpStreamLayer` — the production transport,
//!   a framed JSON RPC protocol over plain TCP.
//!
//! The original Go code also shipped a WebRTC transport with a WAMP signalling
//! client; per project direction those are dropped in favour of the custom
//! TCP-based protocol implemented in `net_transport.rs`, modelled on the
//! framing already used by `drivers/network/federation` and
//! `drivers/network/client/tcp`.

pub mod commands;
pub mod inmem_transport;
pub mod net_transport;
pub mod rpc;
pub mod stream_layer;
pub mod tcp_stream_layer;
pub mod tcp_transport;
pub mod transport;

#[cfg(test)]
mod transport_tests;

pub use commands::{
    EagerSyncRequest, EagerSyncResponse, FastForwardRequest, FastForwardResponse, JoinRequest,
    JoinResponse, SyncRequest, SyncResponse,
};
pub use inmem_transport::InmemTransport;
pub use net_transport::{NetworkTransport, RPC_EAGER_SYNC, RPC_FAST_FORWARD, RPC_JOIN, RPC_SYNC};
pub use rpc::{Rpc, RpcCommand, RpcResponse, RpcResponseKind};
pub use stream_layer::{Conn, StreamLayer};
pub use tcp_stream_layer::{TcpConn, TcpStreamLayer};
pub use tcp_transport::new_tcp_transport;
pub use transport::Transport;
