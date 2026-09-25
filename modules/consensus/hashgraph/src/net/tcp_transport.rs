//! Translation of `chain/net/tcp_transport.go`.
//!
//! Convenience constructor that wires a [`TcpStreamLayer`] into a
//! [`NetworkTransport`] and starts the accept loop.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use super::net_transport::NetworkTransport;
use super::tcp_stream_layer::TcpStreamLayer;
use crate::logrus::Entry;

/// Creates a [`NetworkTransport`] over plain TCP. The returned `Arc` already
/// has its accept loop running.
pub fn new_tcp_transport(
    bind_addr: &str,
    advertise: &str,
    max_pool: usize,
    timeout: Duration,
    join_timeout: Duration,
    logger: Option<Entry>,
) -> Result<Arc<NetworkTransport>> {
    let stream = Arc::new(TcpStreamLayer::new(bind_addr, advertise)?);
    let trans = NetworkTransport::new(stream, max_pool, timeout, join_timeout, logger);
    trans.start_listening();
    Ok(trans)
}
