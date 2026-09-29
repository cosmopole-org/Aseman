//! How requests reach the node: the public HTTP contract ([`http`]), the
//! signed-packet transports ([`shell`], TCP and WebSocket), the main chain
//! ([`chain`]), node-to-node federation ([`federation`]), public files
//! ([`storage_http`]), and module administration ([`admin`]).

pub(crate) mod admin;
pub(crate) mod chain;
pub(crate) mod federation;
pub(crate) mod http;
mod network;
pub(crate) mod shell;
pub(crate) mod storage_http;

pub(crate) use network::Network;
