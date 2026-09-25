//! Translation of `chain/net/stream_layer.go`.
//!
//! Go's `StreamLayer` embedded `net.Listener` and added `Dial` /
//! `AdvertiseAddr`. The translation keeps the same shape but replaces the
//! `net.Conn`/`net.Listener` interfaces with Rust trait objects so a single
//! transport implementation (`NetworkTransport`) can sit on top of any
//! synchronous byte stream — TCP, in-memory tests, or any future transport
//! (e.g. a TLS-wrapped TCP stream).
//!
//! Phase 2.13 of the translation plan originally called for porting Babble's
//! WebRTC / WAMP signalling stack too. Per project direction we drop both and
//! provide only the TCP-based transport: hashgraph nodes interact through a
//! simple framed TCP RPC protocol modelled on the federation/client
//! sockets.

use std::io::{Read, Write};
use std::time::Duration;

use anyhow::Result;

/// A bidirectional, owned byte stream produced by [`StreamLayer`].
///
/// Mirrors Go's `net.Conn`, but limited to the read/write surface
/// `NetworkTransport` actually uses.
pub trait Conn: Read + Write + Send {
    /// Closes the connection.
    fn close(&mut self) -> Result<()>;
}

/// Provides the low-level stream abstraction used by [`NetworkTransport`].
///
/// In Go this composed `net.Listener` plus `Dial` and `AdvertiseAddr`. The
/// trait below collapses that into a single Rust interface.
pub trait StreamLayer: Send + Sync {
    /// Block until the next inbound connection arrives.
    fn accept(&self) -> Result<Box<dyn Conn>>;

    /// Open a new outbound connection to `address`, respecting `timeout`.
    fn dial(&self, address: &str, timeout: Duration) -> Result<Box<dyn Conn>>;

    /// The publicly-reachable address peers should use to contact us.
    fn advertise_addr(&self) -> String;

    /// The local bind address.
    fn addr(&self) -> String;

    /// Close the listener; subsequent `accept` calls return an error.
    fn close(&self) -> Result<()>;
}
