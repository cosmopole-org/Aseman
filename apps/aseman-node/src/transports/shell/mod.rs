//! The signed-packet client transports: TLS TCP and WebSocket, carrying the
//! length-prefixed framing clients (mobile, desktop, CLI) speak. Both hand each
//! packet to the shared session ([`session`]).

mod session;
pub mod tcp;
pub mod ws;

pub use tcp::Tcp;
pub use ws::Ws;
