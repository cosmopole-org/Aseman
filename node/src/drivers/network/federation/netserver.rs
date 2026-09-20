//! Translation of `drivers/network/federation/netserver.go`.
//!
//! Listening side of the federation channel — accept TLS TCP connections,
//! parse `OriginPacket`s from the framed wire format, and dispatch them to
//! a `FedApi` bridge supplied by [`crate::drivers::network::federation::FedNet`].
//!
//! ## Concurrency model — MPSC outbound queue
//!
//! Each federation connection is pinned to a single dedicated I/O thread that
//! directly owns the `TlsStream`.  External writers push encoded frames onto a
//! per-socket `mpsc::Sender`; the I/O thread drains that queue on every loop
//! iteration, interleaved with short-timeout reads.
//!
//! This eliminates the critical deadlock present in the original design, where
//! `listen_for_packets` held `socket.inner` (a `Mutex<SocketInner>`) while
//! blocking on `read_length_prefixed_frame`, making it impossible for any
//! concurrent write (originating from a different thread) to acquire the same
//! lock — the TCP connection could neither send its first request nor receive
//! a response.
//!
//! ## Error recovery
//!
//! * Accept-loop: any individual `accept` error is logged and retried; the
//!   listener itself is never abandoned.
//! * Per-connection I/O: any read or write error terminates that connection
//!   cleanly; the socket is removed from the sockets registry; subsequent
//!   calls to `enqueue` are silent no-ops.
//! * Outbound dial: up to 3 retries with exponential backoff (150 ms →
//!   300 ms → 600 ms) before returning `None`.

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use dashmap::DashMap;

use crate::drivers::network::framing::{
    accept, bind_tls, decode_request_body, decode_response_body, decode_update_body, dial,
    encode_fed_response_body, encode_fed_update_body, encode_request_body,
    write_length_prefixed_frame, TlsStream,
};
use crate::models::core::ICore;
use crate::models::packet::OriginPacket;
use crate::models::ports::network::TlsConfig;
use crate::shell::utils::crypto::secure_unique_string;

// ── Per-connection socket ──────────────────────────────────────────────────

/// Frames the I/O thread accepts from external writers.
enum OutboundFrame {
    Body(Vec<u8>),
    Shutdown,
}

/// Per-connection state.  The owning `TlsStream` lives inside the I/O thread;
/// everything reachable through this struct is safe to call from any thread.
pub struct Socket {
    pub id: String,
    peer: String,
    disconnected: AtomicBool,
    outbound: Mutex<Option<Sender<OutboundFrame>>>,
}

impl Socket {
    fn new(peer: String, outbound: Sender<OutboundFrame>) -> Arc<Socket> {
        Arc::new(Socket {
            id: secure_unique_string(),
            peer,
            disconnected: AtomicBool::new(false),
            outbound: Mutex::new(Some(outbound)),
        })
    }

    pub fn peer_ip(&self) -> String {
        self.peer
            .rsplit_once(':')
            .map(|(a, _)| a.to_string())
            .unwrap_or_else(|| self.peer.clone())
    }

    /// Queue a frame for the I/O thread.  Silent no-op if disconnected.
    fn enqueue(&self, frame: Vec<u8>) {
        if self.disconnected.load(Ordering::Acquire) {
            return;
        }
        let guard = self.outbound.lock().unwrap();
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(OutboundFrame::Body(frame));
        }
    }

    /// Idempotent cooperative shutdown.
    fn shutdown(&self) {
        if self.disconnected.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut guard = self.outbound.lock().unwrap();
        if let Some(tx) = guard.take() {
            let _ = tx.send(OutboundFrame::Shutdown);
        }
    }

    // ── Public outbound encoders ──────────────────────────────────────────

    pub fn write_request(
        &self,
        request_id: &str,
        user_id: &str,
        path: &str,
        payload: &[u8],
        signature: &str,
    ) {
        let frame = encode_request_body(signature, user_id, path, request_id, payload);
        self.enqueue(frame);
    }

    pub fn write_response(&self, request_id: &str, res_code: i64, signature: &str, payload: &[u8]) {
        let frame = encode_fed_response_body(request_id, res_code, signature, payload);
        self.enqueue(frame);
    }

    pub fn write_update(
        &self,
        key: &str,
        target_type: &str,
        target_id_val: &str,
        exceptions: &[String],
        signature: &str,
        payload: &[u8],
    ) {
        let target_id = format!("{}::{}", target_type, target_id_val);
        let frame = encode_fed_update_body(signature, &target_id, exceptions, key, payload);
        self.enqueue(frame);
    }
}

// ── Federation TCP server ──────────────────────────────────────────────────

/// Bridge callback signature: `fn(socket, srcIp, OriginPacket)`.
pub type FedApi = Arc<dyn Fn(Arc<Socket>, String, OriginPacket) + Send + Sync>;

/// Federation TLS-TCP server.
pub struct Tcp {
    app: Arc<dyn ICore>,
    bridge: Mutex<Option<FedApi>>,
    sockets: Arc<DashMap<String, Arc<Socket>>>,
}

impl Tcp {
    pub fn new(app: Arc<dyn ICore>) -> Arc<Tcp> {
        Arc::new(Tcp {
            app,
            bridge: Mutex::new(None),
            sockets: Arc::new(DashMap::new()),
        })
    }

    pub fn inject_bridge(&self, bridge: FedApi) {
        *self.bridge.lock().unwrap() = Some(bridge);
    }

    /// Open a new outbound socket toward `dest_address`.
    ///
    /// Retries up to 3 times with exponential backoff (150 ms → 300 ms →
    /// 600 ms) before returning `None`.
    pub fn new_outbound_socket(
        self: &Arc<Self>,
        dest_address: &str,
        tls_config: Option<&TlsConfig>,
    ) -> Option<Arc<Socket>> {
        let mut cfg = tls_config.cloned();
        if let Some(ref mut c) = cfg {
            if c.server_name.is_empty() {
                c.server_name = dest_address
                    .split(':')
                    .next()
                    .unwrap_or(dest_address)
                    .to_string();
            }
        }

        let mut delay = Duration::from_millis(150);
        for attempt in 0..3u32 {
            match dial(dest_address, cfg.as_ref()) {
                Ok(stream) => {
                    return Some(self.register_connection(stream));
                }
                Err(e) => {
                    if attempt < 2 {
                        eprintln!(
                            "federation dial {} (attempt {}): {} — retrying in {:?}",
                            dest_address,
                            attempt + 1,
                            e,
                            delay
                        );
                        thread::sleep(delay);
                        delay *= 2;
                    } else {
                        eprintln!("federation dial {}: {} — giving up", dest_address, e);
                    }
                }
            }
        }
        None
    }

    /// Register an already-established connection (inbound or outbound),
    /// spawn its I/O thread, and return the `Arc<Socket>`.
    fn register_connection(self: &Arc<Self>, stream: TlsStream) -> Arc<Socket> {
        let peer = stream.peer_addr();
        let (tx, rx) = mpsc::channel::<OutboundFrame>();
        let socket = Socket::new(peer, tx);

        let ip = socket.peer_ip();
        if !ip.is_empty() {
            self.sockets.insert(ip, socket.clone());
        }

        let trans = self.clone();
        let sock = socket.clone();
        thread::spawn(move || trans.run_io_loop(sock, stream, rx));

        socket
    }

    /// Per-connection I/O loop.  Owns the `TlsStream` for the lifetime of the
    /// connection.
    ///
    /// A 20 ms read timeout lets the loop drain outbound frames without ever
    /// blocking writers.  Frames are written immediately as they arrive on the
    /// MPSC channel (no ACK gate needed at the federation level — each
    /// federation socket typically carries one request per session, and
    /// response traffic flows on a separate connection).
    fn run_io_loop(
        self: Arc<Self>,
        socket: Arc<Socket>,
        mut stream: TlsStream,
        rx: Receiver<OutboundFrame>,
    ) {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(20)));

        let mut outbound: VecDeque<Vec<u8>> = VecDeque::new();
        let mut shutdown_requested = false;

        // Resumable length-prefix accumulator — same as tcp.rs.
        let mut len_buf = [0u8; 4];
        let mut len_filled = 0usize;
        let mut body_buf: Vec<u8> = Vec::new();
        let mut body_filled = 0usize;
        let mut expected_len: Option<u32> = None;
        const MAX_FRAME: u32 = 20 * 1024 * 1024;

        'io: loop {
            // 1) Drain outbound MPSC channel into local VecDeque.
            loop {
                match rx.try_recv() {
                    Ok(OutboundFrame::Body(b)) => outbound.push_back(b),
                    Ok(OutboundFrame::Shutdown) => shutdown_requested = true,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        shutdown_requested = true;
                        break;
                    }
                }
            }

            if shutdown_requested && outbound.is_empty() {
                break 'io;
            }

            // 2) Send all pending outbound frames eagerly — no ACK gate needed
            //    at the federation level.
            while let Some(frame) = outbound.front() {
                match write_length_prefixed_frame(&mut stream, frame) {
                    Ok(()) => {
                        outbound.pop_front();
                    }
                    Err(_) => break 'io,
                }
            }

            // 3) Try to read inbound bytes (non-blocking with 20 ms timeout).
            let read_outcome = if expected_len.is_none() {
                let target = &mut len_buf[len_filled..];
                fed_read_into(&mut stream, target)
            } else {
                let target = &mut body_buf[body_filled..];
                fed_read_into(&mut stream, target)
            };

            match read_outcome {
                FedReadOutcome::Bytes(n) => {
                    if expected_len.is_none() {
                        len_filled += n;
                        if len_filled == 4 {
                            let len = u32::from_be_bytes(len_buf);
                            if len > MAX_FRAME {
                                break 'io;
                            }
                            expected_len = Some(len);
                            body_buf = vec![0u8; len as usize];
                            body_filled = 0;
                            len_filled = 0;
                            if len == 0 {
                                expected_len = None;
                                // Zero-length frame — no-op for federation.
                            }
                        }
                    } else {
                        body_filled += n;
                        if body_filled == body_buf.len() {
                            let frame = std::mem::take(&mut body_buf);
                            expected_len = None;
                            body_filled = 0;
                            self.dispatch_frame(&socket, frame);
                        }
                    }
                }
                FedReadOutcome::Idle => {
                    // Short-timeout expiry — loop back to outbound drain.
                }
                FedReadOutcome::Closed | FedReadOutcome::Error => break 'io,
            }
        }

        // Cleanup.
        socket.shutdown();
        let ip = socket.peer_ip();
        if !ip.is_empty() {
            self.sockets.remove(&ip);
        }
    }

    /// Decode and dispatch a fully-read inbound frame.
    fn dispatch_frame(self: &Arc<Self>, socket: &Arc<Socket>, body: Vec<u8>) {
        if body.is_empty() {
            return;
        }

        let pack = match body[0] {
            0x01 => {
                let frame = match decode_update_body(&body[1..], true) {
                    Ok(p) => p,
                    Err(_) => return,
                };
                let mut user_id = String::new();
                let mut store_id = String::new();
                if let Some(("user", id)) = frame.target_id.split_once("::") {
                    user_id = id.to_string();
                } else if let Some(("store", id)) = frame.target_id.split_once("::") {
                    store_id = id.to_string();
                }
                OriginPacket {
                    typ: "update".into(),
                    key: frame.key,
                    user_id,
                    store_id,
                    res_code: 0,
                    binary: frame.payload,
                    signature: frame.signature,
                    request_id: String::new(),
                    exceptions: frame.exceptions,
                }
            }
            0x02 => {
                let frame = match decode_response_body(&body[1..], true) {
                    Ok(p) => p,
                    Err(_) => return,
                };
                OriginPacket {
                    typ: "response".into(),
                    key: String::new(),
                    user_id: String::new(),
                    store_id: String::new(),
                    res_code: frame.res_code,
                    binary: frame.payload,
                    signature: frame.signature,
                    request_id: frame.packet_id,
                    exceptions: Vec::new(),
                }
            }
            0x03 => {
                let frame = match decode_request_body(&body[1..]) {
                    Ok(p) => p,
                    Err(_) => return,
                };
                OriginPacket {
                    typ: "request".into(),
                    key: frame.path,
                    user_id: frame.user_id,
                    store_id: String::new(),
                    res_code: 0,
                    binary: frame.payload,
                    signature: frame.signature,
                    request_id: frame.packet_id,
                    exceptions: Vec::new(),
                }
            }
            _ => return,
        };

        let bridge = self.bridge.lock().unwrap().clone();
        if let Some(bridge) = bridge {
            bridge(socket.clone(), socket.peer_ip(), pack);
        }
    }

    /// Begin the TLS-listen loop on `port`.
    pub fn listen(self: &Arc<Self>, port: i64, tls_config: Option<TlsConfig>) {
        let trans = self.clone();
        thread::spawn(move || {
            let cfg_ref = tls_config.as_ref();
            let (listener, server) = match bind_tls(port, cfg_ref) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("federation listen :{}: {}", port, e);
                    return;
                }
            };
            loop {
                let stream = match accept(&listener, server.as_ref()) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("federation accept: {}", e);
                        continue; // retry — don't abandon the listener
                    }
                };
                trans.register_connection(stream);
            }
        });
    }
}

// ── Non-blocking read helper ───────────────────────────────────────────────

enum FedReadOutcome {
    Bytes(usize),
    Idle,
    Closed,
    Error,
}

fn fed_read_into(stream: &mut TlsStream, buf: &mut [u8]) -> FedReadOutcome {
    use std::io::Read;
    if buf.is_empty() {
        return FedReadOutcome::Idle;
    }
    match stream.read(buf) {
        Ok(0) => FedReadOutcome::Closed,
        Ok(n) => FedReadOutcome::Bytes(n),
        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
            FedReadOutcome::Idle
        }
        Err(e) if e.kind() == ErrorKind::Interrupted => FedReadOutcome::Idle,
        Err(_) => FedReadOutcome::Error,
    }
}

#[allow(dead_code)]
fn _force_use() -> Result<()> {
    Ok(())
}
