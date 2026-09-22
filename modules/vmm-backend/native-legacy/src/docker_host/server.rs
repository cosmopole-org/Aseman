//! Per-connection I/O loop for the docker-host bridge gateway.
//!
//! Each connection is pinned to a dedicated I/O thread that owns its
//! `TcpStream`; a 20 ms read timeout lets that thread interleave inbound reads
//! with draining the outbound MPSC queue, so signal pushes and host-call
//! responses never block on, or deadlock against, the read path. This mirrors
//! the proven concurrency model of the federation `netserver`.
//!
//! ## Security: identity is derived from the source IP, never container-declared
//!
//! On `HELLO` the node identifies the connection from its docker-network source
//! IP (the gateway's `identify`): docker reports which
//! container owns that IP, and the node maps the container name to the identity
//! it recorded at launch. The container sends, and holds, nothing identifying —
//! and it cannot forge its bridge IP — so it can never claim another VM's
//! identity. Every subsequent request is stamped with the resolved identity.
//! This gives docker creatures the same security posture the in-process wasm
//! runtime already has (where `host_call` stamps the node-created runtime's own
//! `machine_id`/`vm_id`).

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use crate::docker_host::connection::{ContainerIdentity, GatewayConnection, OutboundFrame};
use crate::docker_host::dispatch::dispatch_host_call;
use crate::docker_host::gateway::DockerHostGateway;
use crate::docker_host::prelude::*;
use crate::docker_host::protocol::{
    GatewayMessage, MAX_FRAME, MessageAssembler, Opcode, encode_json_message, encode_message,
    next_message_id,
};

/// Run the I/O loop for one accepted container connection.
pub(crate) fn run_connection(gateway: Arc<DockerHostGateway>, stream: TcpStream) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    // Bare source IP (no port), matching docker's reported container IP. This is
    // the connection's authoritative identity key — see `handle_message`.
    let peer_ip = stream
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();
    let _ = stream.set_read_timeout(Some(Duration::from_millis(20)));
    let _ = stream.set_nodelay(true);
    let mut stream = stream;

    // All outbound frames — WELCOME/PONG/RESPONSE (from worker threads) and
    // pushed SIGNALs (from the registry) — funnel through this one channel.
    let (tx, rx): (Sender<OutboundFrame>, Receiver<OutboundFrame>) = mpsc::channel();

    let mut session: Option<Arc<GatewayConnection>> = None;
    let mut assembler = MessageAssembler::new();

    // Resumable length-prefix accumulator (identical scheme to netserver/tcp).
    let mut len_buf = [0u8; 4];
    let mut len_filled = 0usize;
    let mut body_buf: Vec<u8> = Vec::new();
    let mut body_filled = 0usize;
    let mut expected_len: Option<usize> = None;

    let mut outbound: VecDeque<Vec<u8>> = VecDeque::new();
    let mut shutdown_requested = false;

    'io: loop {
        // 1) Drain the outbound MPSC channel into the local queue.
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

        // 2) Flush pending outbound frames eagerly.
        while let Some(frame) = outbound.front() {
            match stream.write_all(frame) {
                Ok(()) => {
                    outbound.pop_front();
                }
                Err(_) => break 'io,
            }
        }

        // 3) Read inbound bytes (non-blocking, 20 ms timeout).
        let read_target: &mut [u8] = if expected_len.is_none() {
            &mut len_buf[len_filled..]
        } else {
            &mut body_buf[body_filled..]
        };

        match read_into(&mut stream, read_target) {
            ReadOutcome::Bytes(n) => {
                if expected_len.is_none() {
                    len_filled += n;
                    if len_filled == 4 {
                        let len = u32::from_be_bytes(len_buf) as usize;
                        len_filled = 0;
                        if len == 0 {
                            // Zero-length keep-alive frame — ignore.
                            continue;
                        }
                        if len > MAX_FRAME {
                            eprintln!(
                                "[docker-host-gateway] {} oversize frame {} — closing",
                                peer, len
                            );
                            break 'io;
                        }
                        expected_len = Some(len);
                        body_buf = vec![0u8; len];
                        body_filled = 0;
                    }
                } else {
                    body_filled += n;
                    if body_filled == body_buf.len() {
                        let body = std::mem::take(&mut body_buf);
                        expected_len = None;
                        body_filled = 0;
                        match assembler.push_frame(&body) {
                            Ok(Some(msg)) => {
                                if !handle_message(
                                    &gateway,
                                    &peer,
                                    &peer_ip,
                                    &tx,
                                    &mut session,
                                    msg,
                                ) {
                                    break 'io;
                                }
                            }
                            Ok(None) => { /* awaiting more chunks */ }
                            Err(e) => {
                                eprintln!("[docker-host-gateway] {} frame error: {}", peer, e);
                                break 'io;
                            }
                        }
                    }
                }
            }
            ReadOutcome::Idle => { /* timeout — loop back to outbound drain */ }
            ReadOutcome::Closed | ReadOutcome::Error => break 'io,
        }
    }

    // Cleanup.
    if let Some(conn) = session.take() {
        conn.shutdown();
        gateway.registry.remove(conn.conn_id);
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Handle one fully-reassembled inbound message. Returns `false` to terminate
/// the connection.
fn handle_message(
    gateway: &Arc<DockerHostGateway>,
    peer: &str,
    peer_ip: &str,
    tx: &Sender<OutboundFrame>,
    session: &mut Option<Arc<GatewayConnection>>,
    msg: GatewayMessage,
) -> bool {
    match msg.opcode {
        Opcode::Hello => {
            if session.is_some() {
                // Duplicate handshake — ignore, keep the existing session.
                return true;
            }
            // The container declares NOTHING about its identity. The node
            // resolves it from the connection's docker-network source IP:
            // IP → (docker) container name → registered identity. A container
            // cannot forge its bridge IP, making this spoof-resistant.
            let resolved = (gateway.identify)(peer_ip);
            let Some(ContainerIdentity {
                vm_id,
                creature_id,
                program_id,
                machine_id,
                entity_id,
            }) = resolved
            else {
                send_local(
                    tx,
                    Opcode::Error,
                    msg.correlation_id,
                    &json!({
                        "ok": false,
                        "error": format!("could not identify a docker creature for source ip {}", peer_ip),
                    }),
                );
                return false;
            };
            let identity = ContainerIdentity {
                vm_id: vm_id.clone(),
                machine_id: machine_id.clone(),
                creature_id: creature_id.clone(),
                program_id: program_id.clone(),
                entity_id: entity_id.clone(),
            };
            let conn_id = gateway.registry.alloc_conn_id();
            let conn = GatewayConnection::new(conn_id, identity, tx.clone());
            gateway.registry.insert(conn.clone());
            *session = Some(conn);
            // Kept for the post-WELCOME flush below, since the WELCOME payload
            // consumes the `machine_id` binding.
            let flush_machine = machine_id.clone();
            let flush_entity = entity_id.clone();
            eprintln!(
                "[docker-host-gateway] {} authenticated vm={} machine={} (conns={})",
                peer,
                vm_id,
                machine_id,
                gateway.registry.count()
            );
            // Tell the container its node-assigned identity (read-only) so it can
            // address replies without ever declaring identity itself.
            send_local(
                tx,
                Opcode::Welcome,
                msg.correlation_id,
                &json!({
                    "ok": true,
                    "sessionId": conn_id,
                    "vmId": vm_id,
                    "machineId": machine_id,
                    "creatureId": creature_id,
                    "programId": program_id,
                }),
            );
            // The container is registered and reachable now: release the cold-spawn
            // slot and hand it every signal that queued while it was cold, in FIFO
            // order, over this fresh connection. Delivering after WELCOME keeps the
            // queued packets ahead of any signal that arrives live from here on.
            gateway
                .registry
                .clear_cold_spawn(&flush_machine, &flush_entity);
            let flushed = gateway
                .registry
                .flush_pending_signals(&flush_machine, &flush_entity);
            if flushed > 0 {
                eprintln!(
                    "[docker-host-gateway] delivered {} queued signal(s) to machine={} entity={}",
                    flushed, flush_machine, flush_entity
                );
            }
            true
        }
        Opcode::Request => {
            let Some(conn) = session.clone() else {
                send_local(
                    tx,
                    Opcode::Error,
                    msg.correlation_id,
                    &json!({ "ok": false, "error": "send HELLO before REQUEST" }),
                );
                return false;
            };
            // Run the (potentially blocking) host call off the I/O thread so the
            // read loop stays responsive and host calls can run concurrently.
            let tx = tx.clone();
            let correlation_id = msg.correlation_id;
            let request = msg.json();
            let identity = conn.identity.clone();
            let host = gateway.host.clone();
            thread::spawn(move || {
                let bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    dispatch_host_call(&*host, &identity, &request)
                }))
                .unwrap_or_else(|_| {
                    json!({ "ok": false, "error": "host call panicked" })
                        .to_string()
                        .into_bytes()
                });
                let frames =
                    encode_message(Opcode::Response, next_message_id(), correlation_id, &bytes);
                for frame in frames {
                    if tx.send(OutboundFrame::Body(frame)).is_err() {
                        break;
                    }
                }
            });
            true
        }
        Opcode::Ping => {
            send_local(tx, Opcode::Pong, msg.correlation_id, &json!({ "ok": true }));
            true
        }
        // Containers never send these; ignore defensively.
        Opcode::Welcome | Opcode::Response | Opcode::Signal | Opcode::Pong | Opcode::Error => true,
    }
}

/// Encode a control message and enqueue it on this connection's outbound queue.
fn send_local(tx: &Sender<OutboundFrame>, opcode: Opcode, correlation_id: u64, value: &JsonValue) {
    let frames = encode_json_message(opcode, next_message_id(), correlation_id, value);
    for frame in frames {
        let _ = tx.send(OutboundFrame::Body(frame));
    }
}

enum ReadOutcome {
    Bytes(usize),
    Idle,
    Closed,
    Error,
}

fn read_into(stream: &mut TcpStream, buf: &mut [u8]) -> ReadOutcome {
    if buf.is_empty() {
        return ReadOutcome::Idle;
    }
    match stream.read(buf) {
        Ok(0) => ReadOutcome::Closed,
        Ok(n) => ReadOutcome::Bytes(n),
        Err(e)
            if e.kind() == ErrorKind::WouldBlock
                || e.kind() == ErrorKind::TimedOut
                || e.kind() == ErrorKind::Interrupted =>
        {
            ReadOutcome::Idle
        }
        Err(_) => ReadOutcome::Error,
    }
}
