//! Translation of `chain/net/tcp_stream_layer.go`.
//!
//! Wraps a `std::net::TcpListener` so the generic [`NetworkTransport`] can run
//! on top of plain TCP. WAMP/WebRTC stream layers were intentionally dropped;
//! hashgraph nodes talk to each other over the framed RPC protocol implemented
//! by `NetworkTransport`.
//!
//! [`NetworkTransport`]: super::net_transport::NetworkTransport

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{Result, anyhow};

use super::stream_layer::{Conn, StreamLayer};

/// `net.Conn` adapter around a `TcpStream`.
pub struct TcpConn {
    stream: TcpStream,
}

impl TcpConn {
    fn new(stream: TcpStream) -> Self {
        TcpConn { stream }
    }
}

impl Read for TcpConn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.stream.read(buf)
    }
}

impl Write for TcpConn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stream.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

impl Conn for TcpConn {
    fn close(&mut self) -> Result<()> {
        // Shutdown is best-effort; ignore EBADF / ENOTCONN.
        let _ = self.stream.shutdown(Shutdown::Both);
        Ok(())
    }
}

/// Plain-TCP [`StreamLayer`].
pub struct TcpStreamLayer {
    advertise: String,
    listener: TcpListener,
}

impl TcpStreamLayer {
    /// Binds a listener on `bind_addr`. If `advertise_addr` is non-empty it is
    /// used as the publicly-reachable address; otherwise the listener's local
    /// address is reported.
    pub fn new(bind_addr: &str, advertise_addr: &str) -> Result<Self> {
        let listener = TcpListener::bind(bind_addr)
            .map_err(|e| anyhow!("tcp listen on {}: {}", bind_addr, e))?;

        let advertise = if advertise_addr.is_empty() {
            listener
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_default()
        } else {
            // Validate the advertise address resolves to a usable socket addr.
            let addr = advertise_addr
                .to_socket_addrs()
                .map_err(|e| anyhow!("resolve advertise addr {}: {}", advertise_addr, e))?
                .next()
                .ok_or_else(|| anyhow!("resolve advertise addr {}: no result", advertise_addr))?;
            if addr.ip().is_unspecified() {
                return Err(anyhow!("local bind address is not advertisable"));
            }
            advertise_addr.to_string()
        };

        Ok(TcpStreamLayer {
            advertise,
            listener,
        })
    }
}

impl StreamLayer for TcpStreamLayer {
    fn accept(&self) -> Result<Box<dyn Conn>> {
        let (stream, _) = self
            .listener
            .accept()
            .map_err(|e| anyhow!("accept: {}", e))?;
        Ok(Box::new(TcpConn::new(stream)))
    }

    fn dial(&self, address: &str, timeout: Duration) -> Result<Box<dyn Conn>> {
        let addr = address
            .to_socket_addrs()
            .map_err(|e| anyhow!("resolve {}: {}", address, e))?
            .next()
            .ok_or_else(|| anyhow!("resolve {}: no result", address))?;
        let stream = TcpStream::connect_timeout(&addr, timeout)
            .map_err(|e| anyhow!("dial {}: {}", address, e))?;
        Ok(Box::new(TcpConn::new(stream)))
    }

    fn advertise_addr(&self) -> String {
        if !self.advertise.is_empty() {
            self.advertise.clone()
        } else {
            self.listener
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_default()
        }
    }

    fn addr(&self) -> String {
        self.listener
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_default()
    }

    fn close(&self) -> Result<()> {
        // `std::net::TcpListener` has no `close` — drop the file descriptor
        // by shutting down the socket via libc. The clone+drop trick used by
        // `accept`'s blocking thread is implemented by making the listener
        // non-blocking before drop happens; here we simply rely on the
        // underlying socket dropping when the layer is dropped. Until then,
        // tickle `accept` by connecting to ourselves so the blocking call
        // returns and the consumer thread observes the shutdown flag.
        if let Ok(local) = self.listener.local_addr() {
            let _ = std::net::TcpStream::connect_timeout(&local, Duration::from_millis(100));
        }
        Ok(())
    }
}
