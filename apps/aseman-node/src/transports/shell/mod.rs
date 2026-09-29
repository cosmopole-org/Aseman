//! Translation of `drivers/network/client` — the user-facing TCP and
//! WebSocket transports the Caspar shell uses to ferry requests between
//! clients (mobile / desktop / CLI) and the node.

mod session;
pub mod tcp;
pub mod ws;

pub use tcp::Tcp;
pub use ws::Ws;
