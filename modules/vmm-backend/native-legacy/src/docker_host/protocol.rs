//! Wire protocol for the docker-host bridge gateway.
//!
//! Docker-based creature containers do **all** of their interactions with the
//! outside world over a single long-lived TCP connection to this gateway. The
//! protocol is a small, self-describing, **chunked** framing scheme so that
//! arbitrarily large payloads (file blobs, HTTP bodies, DB values) can be
//! streamed in fixed-size pieces in either direction without buffering an
//! entire message on the wire at once.
//!
//! ## Frame layout
//!
//! Every frame is length-delimited on the wire:
//!
//! ```text
//! ┌────────────┬─────────────────────────────────────────────────────────┐
//! │ u32  BE    │ frame body (HEADER_LEN + chunk bytes)                     │
//! │ frame_len  │                                                           │
//! └────────────┴─────────────────────────────────────────────────────────┘
//!
//! frame body:
//! ┌──────┬───────────┬────────────────┬────────┬────────┬────────────────┐
//! │ u8   │ u64 BE    │ u64 BE         │ u32 BE │ u32 BE │ chunk bytes     │
//! │ op   │ message_id│ correlation_id │ seq    │ total  │ (<= MAX_CHUNK)  │
//! └──────┴───────────┴────────────────┴────────┴────────┴────────────────┘
//! ```
//!
//! * `op`             — [`Opcode`] identifying the message kind.
//! * `message_id`     — unique per logical message; all chunks share it and are
//!                      reassembled by the receiver.
//! * `correlation_id` — request id echoed back on the matching response so the
//!                      caller can pair a `RESPONSE` with its `REQUEST`.
//! * `seq` / `total`  — chunk index (0-based) and the total number of chunks
//!                      that make up this message.
//!
//! A logical message whose payload fits in a single chunk uses `seq = 0`,
//! `total = 1` — the common case for control frames and small host calls.

use crate::docker_host::prelude::*;

/// Header bytes that precede the chunk payload inside a frame body.
pub(crate) const HEADER_LEN: usize = 1 + 8 + 8 + 4 + 4;

/// Maximum chunk payload carried by a single frame (64 KiB).
pub(crate) const MAX_CHUNK: usize = 64 * 1024;

/// Hard ceiling on a single on-wire frame body — guards against a hostile or
/// corrupt length prefix.
pub(crate) const MAX_FRAME: usize = 32 * 1024 * 1024;

/// Hard ceiling on a fully reassembled logical message (64 MiB).
pub(crate) const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// Message kinds carried over the gateway connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opcode {
    /// container → node: handshake announcing the container's identity.
    Hello,
    /// node → container: handshake acknowledgement.
    Welcome,
    /// container → node: a VM host-function call (`{op, input}`).
    Request,
    /// node → container: the result of a host-function call.
    Response,
    /// node → container: a pushed signal/notification from another creature.
    Signal,
    /// container → node: liveness probe.
    Ping,
    /// node → container: liveness reply.
    Pong,
    /// node → container: a protocol/handshake error (terminates the session).
    Error,
}

impl Opcode {
    pub(crate) fn to_byte(self) -> u8 {
        match self {
            Opcode::Hello => 0x01,
            Opcode::Welcome => 0x02,
            Opcode::Request => 0x10,
            Opcode::Response => 0x11,
            Opcode::Signal => 0x20,
            Opcode::Ping => 0x30,
            Opcode::Pong => 0x31,
            Opcode::Error => 0x40,
        }
    }

    pub(crate) fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0x01 => Opcode::Hello,
            0x02 => Opcode::Welcome,
            0x10 => Opcode::Request,
            0x11 => Opcode::Response,
            0x20 => Opcode::Signal,
            0x30 => Opcode::Ping,
            0x31 => Opcode::Pong,
            0x40 => Opcode::Error,
            _ => return None,
        })
    }
}

/// A fully reassembled logical message.
#[derive(Clone, Debug)]
pub(crate) struct GatewayMessage {
    pub(crate) opcode: Opcode,
    #[allow(dead_code)]
    pub(crate) message_id: u64,
    pub(crate) correlation_id: u64,
    pub(crate) payload: Vec<u8>,
}

impl GatewayMessage {
    /// Decode the payload as JSON, falling back to `Null` on empty/invalid.
    pub(crate) fn json(&self) -> JsonValue {
        if self.payload.is_empty() {
            return JsonValue::Null;
        }
        serde_json::from_slice(&self.payload).unwrap_or(JsonValue::Null)
    }
}

/// Monotonic message-id source for outbound messages on a connection.
pub(crate) fn next_message_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Encode a logical message into one or more length-prefixed wire frames,
/// splitting `payload` into [`MAX_CHUNK`]-sized chunks.
///
/// Always emits at least one frame, even for an empty payload (`total = 1`,
/// zero-length chunk), so the receiver can dispatch control messages that carry
/// no body.
pub(crate) fn encode_message(
    opcode: Opcode,
    message_id: u64,
    correlation_id: u64,
    payload: &[u8],
) -> Vec<Vec<u8>> {
    let total_chunks = if payload.is_empty() {
        1
    } else {
        payload.len().div_ceil(MAX_CHUNK)
    };

    let mut frames = Vec::with_capacity(total_chunks);
    for seq in 0..total_chunks {
        let start = seq * MAX_CHUNK;
        let end = std::cmp::min(start + MAX_CHUNK, payload.len());
        let chunk = &payload[start..end];

        let body_len = HEADER_LEN + chunk.len();
        let mut frame = Vec::with_capacity(4 + body_len);
        frame.extend_from_slice(&(body_len as u32).to_be_bytes());
        frame.push(opcode.to_byte());
        frame.extend_from_slice(&message_id.to_be_bytes());
        frame.extend_from_slice(&correlation_id.to_be_bytes());
        frame.extend_from_slice(&(seq as u32).to_be_bytes());
        frame.extend_from_slice(&(total_chunks as u32).to_be_bytes());
        frame.extend_from_slice(chunk);
        frames.push(frame);
    }
    frames
}

/// Convenience: encode a JSON value as a complete chunked message.
pub(crate) fn encode_json_message(
    opcode: Opcode,
    message_id: u64,
    correlation_id: u64,
    value: &JsonValue,
) -> Vec<Vec<u8>> {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    encode_message(opcode, message_id, correlation_id, &bytes)
}

/// A single decoded frame header + chunk (one piece of a logical message).
struct FrameChunk {
    opcode: Opcode,
    message_id: u64,
    correlation_id: u64,
    seq: u32,
    total: u32,
    chunk: Vec<u8>,
}

/// Parse a frame body (everything after the 4-byte length prefix) into its
/// header fields and chunk payload.
fn parse_frame_body(body: &[u8]) -> Result<FrameChunk, String> {
    if body.len() < HEADER_LEN {
        return Err(format!(
            "gateway frame too short: {} < {} header bytes",
            body.len(),
            HEADER_LEN
        ));
    }
    let opcode = Opcode::from_byte(body[0])
        .ok_or_else(|| format!("unknown gateway opcode: 0x{:02x}", body[0]))?;
    let message_id = u64::from_be_bytes(body[1..9].try_into().unwrap());
    let correlation_id = u64::from_be_bytes(body[9..17].try_into().unwrap());
    let seq = u32::from_be_bytes(body[17..21].try_into().unwrap());
    let total = u32::from_be_bytes(body[21..25].try_into().unwrap());
    let chunk = body[HEADER_LEN..].to_vec();

    if total == 0 {
        return Err("gateway frame declares total=0 chunks".to_string());
    }
    if seq >= total {
        return Err(format!("gateway frame seq {} >= total {}", seq, total));
    }
    Ok(FrameChunk {
        opcode,
        message_id,
        correlation_id,
        seq,
        total,
        chunk,
    })
}

/// In-flight reassembly state for a single multi-chunk message.
struct PartialMessage {
    opcode: Opcode,
    correlation_id: u64,
    total: u32,
    received: u32,
    chunks: Vec<Option<Vec<u8>>>,
    size: usize,
}

/// Reassembles chunked frames belonging to one connection back into whole
/// [`GatewayMessage`]s. Out-of-order chunks are tolerated; duplicates and
/// oversize messages are rejected.
#[derive(Default)]
pub(crate) struct MessageAssembler {
    partials: HashMap<u64, PartialMessage>,
}

impl MessageAssembler {
    pub(crate) fn new() -> Self {
        Self {
            partials: HashMap::new(),
        }
    }

    /// Feed one frame body. Returns `Ok(Some(msg))` once the final chunk of a
    /// message has arrived and the message is fully reassembled.
    pub(crate) fn push_frame(&mut self, body: &[u8]) -> Result<Option<GatewayMessage>, String> {
        let frame = parse_frame_body(body)?;

        // Single-chunk fast path — no bookkeeping needed.
        if frame.total == 1 {
            return Ok(Some(GatewayMessage {
                opcode: frame.opcode,
                message_id: frame.message_id,
                correlation_id: frame.correlation_id,
                payload: frame.chunk,
            }));
        }

        let entry = self
            .partials
            .entry(frame.message_id)
            .or_insert_with(|| PartialMessage {
                opcode: frame.opcode,
                correlation_id: frame.correlation_id,
                total: frame.total,
                received: 0,
                chunks: vec![None; frame.total as usize],
                size: 0,
            });

        if entry.total != frame.total {
            return Err("gateway chunk total mismatch within a message".to_string());
        }
        let slot = &mut entry.chunks[frame.seq as usize];
        if slot.is_some() {
            // Duplicate chunk — ignore silently rather than corrupt the message.
            return Ok(None);
        }
        entry.size += frame.chunk.len();
        if entry.size > MAX_MESSAGE {
            self.partials.remove(&frame.message_id);
            return Err(format!(
                "gateway message exceeds {} byte ceiling",
                MAX_MESSAGE
            ));
        }
        *slot = Some(frame.chunk);
        entry.received += 1;

        if entry.received < entry.total {
            return Ok(None);
        }

        // Complete — concatenate in order.
        let done = self.partials.remove(&frame.message_id).unwrap();
        let mut payload = Vec::with_capacity(done.size);
        for chunk in done.chunks.into_iter() {
            payload.extend_from_slice(&chunk.unwrap_or_default());
        }
        Ok(Some(GatewayMessage {
            opcode: done.opcode,
            message_id: frame.message_id,
            correlation_id: done.correlation_id,
            payload,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_single_chunk() {
        let frames = encode_message(Opcode::Request, 7, 42, b"hello");
        assert_eq!(frames.len(), 1);
        let mut asm = MessageAssembler::new();
        let body = &frames[0][4..];
        let msg = asm.push_frame(body).unwrap().unwrap();
        assert_eq!(msg.opcode, Opcode::Request);
        assert_eq!(msg.correlation_id, 42);
        assert_eq!(msg.payload, b"hello");
    }

    #[test]
    fn round_trips_multi_chunk_out_of_order() {
        let payload: Vec<u8> = (0..(MAX_CHUNK * 2 + 11)).map(|i| (i % 251) as u8).collect();
        let frames = encode_message(Opcode::Response, 9, 100, &payload);
        assert_eq!(frames.len(), 3);

        let mut asm = MessageAssembler::new();
        // Feed in reverse order; only the last completes the message.
        assert!(asm.push_frame(&frames[2][4..]).unwrap().is_none());
        assert!(asm.push_frame(&frames[0][4..]).unwrap().is_none());
        let msg = asm.push_frame(&frames[1][4..]).unwrap().unwrap();
        assert_eq!(msg.payload, payload);
        assert_eq!(msg.correlation_id, 100);
    }

    #[test]
    fn empty_payload_still_emits_one_frame() {
        let frames = encode_message(Opcode::Ping, 1, 0, b"");
        assert_eq!(frames.len(), 1);
        let mut asm = MessageAssembler::new();
        let msg = asm.push_frame(&frames[0][4..]).unwrap().unwrap();
        assert_eq!(msg.opcode, Opcode::Ping);
        assert!(msg.payload.is_empty());
    }
}
