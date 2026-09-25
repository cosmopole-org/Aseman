//! Translation of `chain/dummy` — the toy application Babble's integration
//! tests run against. Only the in-memory variant is translated; the
//! socket-based dummy is intentionally dropped since the socket app/babble
//! proxies are not part of the Rust port (see the chain `proxy/socket`
//! drop note in `proxy/mod.rs`).

pub mod inmem_dummy;
pub mod state;

#[cfg(test)]
mod tests;

pub use inmem_dummy::InmemDummyClient;
pub use state::State;
