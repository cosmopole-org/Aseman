//! Translation of `chain/net/net_transport.go`.
//!
//! `NetworkTransport` is the generic [`Transport`] implementation that sits on
//! top of a [`StreamLayer`]. Per the project direction the WAMP/WebRTC stack
//! is dropped; this module defines a custom TCP-friendly RPC protocol modelled
//! on the framing used by the `drivers/network/federation` and
//! `drivers/network/client/tcp` servers:
//!
//! ```text
//!  request  = u32be(len) || u8(rpc_type) || json(request_body)
//!  response = u32be(len) || u8(status)   || json(response_body | err_string)
//! ```
//!
//! `status` is `0x00` for a successful response and `0x01` when the body is an
//! error string. The length prefix counts the trailing bytes (status +
//! payload), not itself. Connections are pooled per target and reused for
//! multiple sequential request/response exchanges.
//!
//! [`Transport`]: super::transport::Transport
//! [`StreamLayer`]: super::stream_layer::StreamLayer

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use serde::{Serialize, de::DeserializeOwned};

use super::commands::{
    EagerSyncRequest, EagerSyncResponse, FastForwardRequest, FastForwardResponse, JoinRequest,
    JoinResponse, SyncRequest, SyncResponse,
};
use super::rpc::{Rpc, RpcCommand, RpcResponse, RpcResponseKind};
use super::stream_layer::{Conn, StreamLayer};
use super::transport::Transport;
use crate::logrus::Entry;

/// RPC type bytes — identical numeric ordering to the Go original so the
/// translated and Go transports speak the same enum if ever bridged.
pub const RPC_JOIN: u8 = 0;
pub const RPC_SYNC: u8 = 1;
pub const RPC_EAGER_SYNC: u8 = 2;
pub const RPC_FAST_FORWARD: u8 = 3;

/// Response status bytes.
const RESP_OK: u8 = 0;
const RESP_ERR: u8 = 1;

/// Hard cap on packet size, mirrors the federation/client driver behaviour.
const MAX_PACKET_LEN: u32 = 20_000_000;

/// `NetworkTransport` is the framed RPC transport.
///
/// `Arc<Self>` lets the consumer and accept threads keep the transport alive
/// while sharing access to the pool and chain-input map.
pub struct NetworkTransport {
    logger: Entry,
    stream: Arc<dyn StreamLayer>,
    max_pool: usize,
    timeout: Duration,
    join_timeout: Duration,

    // Per-target connection pool.
    conn_pool: Mutex<HashMap<String, Vec<NetConn>>>,

    // Dispatcher channel — connection-handler threads push parsed RPCs here.
    consume_tx: Sender<Rpc>,

    // Per-chain consumer channels (keyed by "<workChainId>::<shardChainId>").
    chain_inputs: Mutex<HashMap<String, (Sender<Rpc>, Receiver<Rpc>)>>,

    // Shutdown flag inspected by the accept/consumer threads.
    shutdown: Mutex<bool>,
    shutdown_tx: Mutex<Option<Sender<()>>>,
    shutdown_rx: Receiver<()>,
}

/// A pooled outbound connection.
struct NetConn {
    target: String,
    conn: Box<dyn Conn>,
}

impl NetConn {
    fn release(&mut self) {
        let _ = self.conn.close();
    }
}

impl NetworkTransport {
    /// Build a new `NetworkTransport` over the given [`StreamLayer`].
    ///
    /// Mirrors `NewNetworkTransport`. The returned value is wrapped in `Arc`
    /// so the accept loop and dispatcher can both hold strong references.
    pub fn new(
        stream: Arc<dyn StreamLayer>,
        max_pool: usize,
        timeout: Duration,
        join_timeout: Duration,
        logger: Option<Entry>,
    ) -> Arc<Self> {
        let logger = logger.unwrap_or_else(Entry::standalone);

        let (consume_tx, consume_rx) = bounded::<Rpc>(512);
        let (shutdown_tx, shutdown_rx) = unbounded::<()>();

        let trans = Arc::new(NetworkTransport {
            logger,
            stream,
            max_pool,
            timeout,
            join_timeout,
            conn_pool: Mutex::new(HashMap::new()),
            consume_tx,
            chain_inputs: Mutex::new(HashMap::new()),
            shutdown: Mutex::new(false),
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            shutdown_rx,
        });

        // Spawn the dispatcher: matches Go's `future.Async` consumer loop. It
        // routes RPCs to the chain-specific consumer channels by chain id.
        let dispatcher_trans = Arc::clone(&trans);
        thread::spawn(move || dispatcher_trans.run_dispatcher(consume_rx));

        trans
    }

    /// Are we shut down?
    pub fn is_shutdown(&self) -> bool {
        *self.shutdown.lock().unwrap()
    }

    fn chain_id_of(command: &RpcCommand) -> String {
        match command {
            RpcCommand::Sync(r) => format!("{}::{}", r.work_chain_id, r.shard_chain_id),
            RpcCommand::EagerSync(r) => format!("{}::{}", r.work_chain_id, r.shard_chain_id),
            RpcCommand::FastForward(r) => format!("{}::{}", r.work_chain_id, r.shard_chain_id),
            RpcCommand::Join(r) => format!("{}::{}", r.work_chain_id, r.shard_chain_id),
        }
    }

    /// The dispatcher thread: forwards inbound RPCs to the per-chain
    /// consumer channel.
    fn run_dispatcher(self: Arc<Self>, rx: Receiver<Rpc>) {
        while let Ok(rpc) = rx.recv() {
            let chain_id = Self::chain_id_of(&rpc.command);
            let target = self
                .chain_inputs
                .lock()
                .unwrap()
                .get(&chain_id)
                .map(|(tx, _)| tx.clone());
            if let Some(tx) = target {
                let _ = tx.send(rpc);
            } else {
                // No consumer registered for that chain — respond with an
                // error so the caller doesn't block forever.
                rpc.respond(None, Some(format!("no consumer for chain {}", chain_id)));
            }
        }
    }

    /// Pop a pooled connection for `target`, if any.
    fn get_pooled_conn(&self, target: &str) -> Option<NetConn> {
        let mut pool = self.conn_pool.lock().unwrap();
        pool.get_mut(target).and_then(|v| v.pop())
    }

    /// Borrow (or dial) a connection to `target`.
    fn get_conn(&self, target: &str, timeout: Duration) -> Result<NetConn> {
        if let Some(c) = self.get_pooled_conn(target) {
            return Ok(c);
        }
        let conn = self.stream.dial(target, timeout)?;
        Ok(NetConn {
            target: target.to_string(),
            conn,
        })
    }

    /// Return a connection to the pool, or release it if the pool is full.
    fn return_conn(&self, mut conn: NetConn) {
        let mut pool = self.conn_pool.lock().unwrap();
        if self.is_shutdown() {
            conn.release();
            return;
        }
        let entry = pool.entry(conn.target.clone()).or_default();
        if entry.len() >= self.max_pool {
            drop(pool);
            conn.release();
        } else {
            entry.push(conn);
        }
    }

    /// The accept loop. Calls `stream.accept()` and spawns a handler thread
    /// per inbound connection.
    fn run_accept_loop(self: Arc<Self>) {
        loop {
            match self.stream.accept() {
                Ok(conn) => {
                    if self.is_shutdown() {
                        return;
                    }
                    let trans = Arc::clone(&self);
                    thread::spawn(move || trans.handle_conn(conn));
                }
                Err(e) => {
                    if self.is_shutdown() {
                        return;
                    }
                    self.logger
                        .with_error(e)
                        .error("Failed to accept connection");
                }
            }
        }
    }

    /// Per-connection handler: read framed RPCs and dispatch them.
    fn handle_conn(self: Arc<Self>, mut conn: Box<dyn Conn>) {
        loop {
            match self.handle_command(&mut conn) {
                Ok(()) => continue,
                Err(e) => {
                    // `unexpected EOF` is the normal close path; only log
                    // unexpected errors at error level.
                    if !is_clean_eof(&e) {
                        self.logger
                            .with_error(&e)
                            .warn("Failed to decode incoming command");
                    }
                    let _ = conn.close();
                    return;
                }
            }
        }
    }

    /// Read a single inbound request from `conn`, dispatch it, and write the
    /// matching response.
    fn handle_command(&self, conn: &mut Box<dyn Conn>) -> Result<()> {
        let frame = read_frame(conn.as_mut())?;
        if frame.is_empty() {
            return Err(anyhow!("empty frame"));
        }
        let rpc_type = frame[0];
        let body = &frame[1..];

        let (resp_tx, resp_rx) = bounded::<RpcResponse>(1);
        let command = match rpc_type {
            RPC_SYNC => {
                let req: SyncRequest = serde_json::from_slice(body)?;
                RpcCommand::Sync(req)
            }
            RPC_EAGER_SYNC => {
                let req: EagerSyncRequest = serde_json::from_slice(body)?;
                RpcCommand::EagerSync(req)
            }
            RPC_FAST_FORWARD => {
                let req: FastForwardRequest = serde_json::from_slice(body)?;
                RpcCommand::FastForward(req)
            }
            RPC_JOIN => {
                let req: JoinRequest = serde_json::from_slice(body)?;
                RpcCommand::Join(req)
            }
            other => return Err(anyhow!("unknown rpc type {}", other)),
        };

        // Hand off to the dispatcher. If we're already shut down, bail out.
        let rpc = Rpc {
            command,
            resp_chan: resp_tx,
        };
        crossbeam_channel::select! {
            send(self.consume_tx, rpc) -> r => {
                r.map_err(|_| anyhow!("transport shutdown"))?;
            }
            recv(self.shutdown_rx) -> _ => {
                return Err(anyhow!("transport shutdown"));
            }
        }

        // Wait for the response.
        let resp = crossbeam_channel::select! {
            recv(resp_rx) -> r => r.map_err(|e| anyhow!("response channel closed: {}", e))?,
            recv(self.shutdown_rx) -> _ => return Err(anyhow!("transport shutdown")),
        };

        write_response(conn.as_mut(), &resp)?;
        Ok(())
    }
}

impl Transport for NetworkTransport {
    fn listen(&self) {
        // The accept loop must hold an `Arc<NetworkTransport>` so it can hand
        // a clone to each per-connection handler thread. We can't construct
        // such an `Arc` from `&self`, so the loop is started at construction
        // time via [`NetworkTransport::start_listening`]; this trait method
        // is intentionally a no-op for parity with the Go signature.
    }

    fn consumer(&self, work_chain_id: &str, shard_chain_id: &str) -> Receiver<Rpc> {
        let key = format!("{}::{}", work_chain_id, shard_chain_id);
        let mut map = self.chain_inputs.lock().unwrap();
        let entry = map.entry(key).or_insert_with(|| bounded::<Rpc>(512));
        entry.1.clone()
    }

    fn local_addr(&self) -> String {
        self.stream.addr()
    }

    fn advertise_addr(&self) -> String {
        self.stream.advertise_addr()
    }

    fn sync(&self, target: &str, args: &SyncRequest) -> Result<SyncResponse> {
        self.generic_rpc(target, RPC_SYNC, self.timeout, args)
    }

    fn eager_sync(&self, target: &str, args: &EagerSyncRequest) -> Result<EagerSyncResponse> {
        self.generic_rpc(target, RPC_EAGER_SYNC, self.timeout, args)
    }

    fn fast_forward(&self, target: &str, args: &FastForwardRequest) -> Result<FastForwardResponse> {
        self.generic_rpc(target, RPC_FAST_FORWARD, self.timeout, args)
    }

    fn join(&self, target: &str, args: &JoinRequest) -> Result<JoinResponse> {
        self.generic_rpc(target, RPC_JOIN, self.join_timeout, args)
    }

    fn close(&self) -> Result<()> {
        let mut shut = self.shutdown.lock().unwrap();
        if !*shut {
            *shut = true;
            // Drop the broadcast sender to wake the consume/accept threads.
            if let Some(tx) = self.shutdown_tx.lock().unwrap().take() {
                drop(tx);
            }
            // Close the underlying listener (best-effort).
            let _ = self.stream.close();

            // Release any pooled connections.
            let mut pool = self.conn_pool.lock().unwrap();
            for (_, v) in pool.drain() {
                for mut c in v {
                    c.release();
                }
            }
        }
        Ok(())
    }
}

impl NetworkTransport {
    /// Spawn the accept loop on a background thread. The caller usually
    /// wires this in instead of calling [`Transport::listen`] directly so
    /// that the loop owns an `Arc<NetworkTransport>`.
    pub fn start_listening(self: &Arc<Self>) {
        let trans = Arc::clone(self);
        thread::spawn(move || trans.run_accept_loop());
    }

    /// Generic single request / single response RPC.
    ///
    /// Retries on connection or I/O errors up to 3 times total with
    /// exponential backoff (100 ms → 200 ms → 400 ms).  Pooled connections
    /// that have gone stale are released on first use and replaced by a fresh
    /// dial on the retry.
    fn generic_rpc<Req, Resp>(
        &self,
        target: &str,
        rpc_type: u8,
        _timeout: Duration,
        args: &Req,
    ) -> Result<Resp>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let body = serde_json::to_vec(args)?;
        let mut frame = Vec::with_capacity(1 + body.len());
        frame.push(rpc_type);
        frame.extend_from_slice(&body);

        let mut last_err = anyhow!("rpc: no attempts made");
        let mut delay = Duration::from_millis(100);

        for attempt in 0..3u32 {
            if self.is_shutdown() {
                return Err(anyhow!("transport shutdown"));
            }

            let mut conn = match self.get_conn(target, _timeout) {
                Ok(c) => c,
                Err(e) => {
                    last_err = e;
                    if attempt < 2 {
                        thread::sleep(delay);
                        delay *= 2;
                    }
                    continue;
                }
            };

            // Write the request frame.
            if let Err(e) = write_frame(conn.conn.as_mut(), &frame) {
                conn.release();
                last_err = e;
                if attempt < 2 {
                    thread::sleep(delay);
                    delay *= 2;
                }
                continue;
            }

            // Read the response frame.
            let resp_frame = match read_frame(conn.conn.as_mut()) {
                Ok(f) => f,
                Err(e) => {
                    conn.release();
                    last_err = e;
                    if attempt < 2 {
                        thread::sleep(delay);
                        delay *= 2;
                    }
                    continue;
                }
            };

            if resp_frame.is_empty() {
                conn.release();
                last_err = anyhow!("empty response frame");
                if attempt < 2 {
                    thread::sleep(delay);
                    delay *= 2;
                }
                continue;
            }

            let status = resp_frame[0];
            let resp_body = &resp_frame[1..];

            return match status {
                RESP_OK => {
                    let resp: Resp = serde_json::from_slice(resp_body)?;
                    self.return_conn(conn);
                    Ok(resp)
                }
                RESP_ERR => {
                    let msg: String = serde_json::from_slice(resp_body)
                        .unwrap_or_else(|_| "unknown error".into());
                    self.return_conn(conn);
                    Err(anyhow!("{}", msg))
                }
                other => {
                    conn.release();
                    Err(anyhow!("unknown response status {}", other))
                }
            };
        }

        Err(last_err)
    }
}

// -- framing helpers --------------------------------------------------------

fn read_exact(r: &mut dyn Read, buf: &mut [u8]) -> Result<()> {
    r.read_exact(buf).map_err(|e| anyhow!("read: {}", e))
}

fn read_frame(r: &mut dyn Read) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    read_exact(r, &mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_PACKET_LEN {
        return Err(anyhow!("frame too large: {}", len));
    }
    let mut body = vec![0u8; len as usize];
    read_exact(r, &mut body)?;
    Ok(body)
}

fn write_frame(w: &mut dyn Write, body: &[u8]) -> Result<()> {
    let len = body.len() as u32;
    if len > MAX_PACKET_LEN {
        return Err(anyhow!("frame too large: {}", len));
    }
    w.write_all(&len.to_be_bytes())
        .map_err(|e| anyhow!("write len: {}", e))?;
    w.write_all(body)
        .map_err(|e| anyhow!("write body: {}", e))?;
    w.flush().map_err(|e| anyhow!("flush: {}", e))?;
    Ok(())
}

fn write_response(w: &mut dyn Write, resp: &RpcResponse) -> Result<()> {
    if let Some(e) = &resp.error {
        let body = serde_json::to_vec(e)?;
        let mut frame = Vec::with_capacity(1 + body.len());
        frame.push(RESP_ERR);
        frame.extend_from_slice(&body);
        return write_frame(w, &frame);
    }
    // Successful response. Encode whichever variant we have; if none is set
    // we still send an OK frame with `null` so the caller doesn't block.
    let body = match &resp.response {
        Some(RpcResponseKind::Sync(r)) => serde_json::to_vec(r)?,
        Some(RpcResponseKind::EagerSync(r)) => serde_json::to_vec(r)?,
        Some(RpcResponseKind::FastForward(r)) => serde_json::to_vec(r)?,
        Some(RpcResponseKind::Join(r)) => serde_json::to_vec(r)?,
        None => serde_json::to_vec(&serde_json::Value::Null)?,
    };
    let mut frame = Vec::with_capacity(1 + body.len());
    frame.push(RESP_OK);
    frame.extend_from_slice(&body);
    write_frame(w, &frame)
}

fn is_clean_eof(e: &anyhow::Error) -> bool {
    let msg = format!("{}", e);
    msg.contains("failed to fill whole buffer") || msg.contains("unexpected end of file")
}
