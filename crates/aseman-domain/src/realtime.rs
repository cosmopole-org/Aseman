//! Durable realtime events (A707, ADR 0014).
//!
//! Delivery is **at least once**. That is not a weakness to be apologised for: it is
//! the only honest promise a system can keep across a crash between "the consumer
//! processed it" and "the consumer recorded that it did". Consumers checkpoint after
//! processing and deduplicate on the event ID.
//!
//! Global total ordering is not promised either. Order is **per stream**, which is
//! what a consumer of one subject actually needs, and what a partitioned log can keep.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Uuid;

/// How long an event is kept, and therefore how far back a consumer may replay.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionClass {
    /// Kept briefly; a consumer that falls behind loses it and must resync.
    Transient,
    /// Kept for the standard replay window.
    Standard,
    /// Kept until an explicit retention decision removes it: an audit trail.
    Durable,
}

impl RetentionClass {
    /// How long this class is kept.
    #[must_use]
    pub const fn millis(self) -> Option<i64> {
        match self {
            Self::Transient => Some(60 * 60 * 1000),
            Self::Standard => Some(7 * 24 * 60 * 60 * 1000),
            Self::Durable => None,
        }
    }
}

/// One event, as it is stored and delivered.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub id: Uuid,
    /// The stream this event is ordered within.
    pub stream: String,
    /// The creature the event belongs to. Authorization is decided against this, never
    /// against anything in the payload.
    pub creature_id: Uuid,
    pub kind: String,
    /// Who produced it.
    pub producer: String,
    /// Monotonic within `stream`, starting at 1. There is no global order.
    pub sequence: u64,
    pub at_millis: i64,
    /// `sha256:{hex}` of the payload.
    pub payload_digest: String,
    pub retention: RetentionClass,
    /// The envelope version, for example `1`.
    pub version: String,
    /// Set when the producer wants a repeated publication to collapse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// Where a consumer has got to in a stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub consumer: String,
    pub stream: String,
    /// The highest sequence this consumer has finished processing.
    pub sequence: u64,
    pub at_millis: i64,
}

/// Whether an event may be appended to a stream whose last sequence is `last`.
///
/// Sequences are dense and monotonic per stream: a gap would make a consumer wait
/// forever for an event that is never coming, and a repeat would make replay
/// ambiguous.
///
/// # Errors
///
/// The reason it may not.
pub fn accept_append(event: &Event, last: Option<u64>) -> Result<(), RealtimeError> {
    if event.version != "1" {
        return Err(RealtimeError::UnknownVersion);
    }
    if event.stream.is_empty() || event.stream.len() > 256 {
        return Err(RealtimeError::InvalidStream);
    }
    if !event
        .payload_digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(RealtimeError::InvalidDigest);
    }
    let expected = last.map_or(1, |last| last + 1);
    if event.sequence != expected {
        return Err(RealtimeError::OutOfOrder { expected });
    }
    Ok(())
}

/// Whether a consumer may move its checkpoint to `sequence`.
///
/// A checkpoint never moves backwards: that would replay events the consumer has
/// already acted on, and at-least-once becomes twice-on-purpose.
///
/// # Errors
///
/// [`RealtimeError::CheckpointWentBackwards`].
pub fn accept_checkpoint(current: Option<u64>, sequence: u64) -> Result<(), RealtimeError> {
    match current {
        Some(current) if sequence < current => Err(RealtimeError::CheckpointWentBackwards),
        _ => Ok(()),
    }
}

/// Whether a consumer may still replay from `sequence`, given the oldest event the
/// stream still holds.
///
/// A consumer that has fallen outside the retention window is told to resync rather
/// than served a silently incomplete stream.
#[must_use]
pub fn can_replay_from(oldest_kept: Option<u64>, sequence: u64) -> bool {
    oldest_kept.is_none_or(|oldest| sequence + 1 >= oldest)
}

/// Whether an event may be delivered to a subscriber of `creature`.
///
/// Discoverability is not delivery: a workload being resolvable in the federation
/// says nothing about which events it may see. Scope is decided by the creature the
/// event belongs to, never by anything in the payload.
#[must_use]
pub fn may_deliver(event: &Event, subscriber_creature: Uuid) -> bool {
    event.creature_id == subscriber_creature
}

/// Whether an event should still be kept at `now_millis`.
#[must_use]
pub fn is_retained(event: &Event, now_millis: i64) -> bool {
    match event.retention.millis() {
        None => true,
        Some(window) => now_millis < event.at_millis.saturating_add(window),
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RealtimeError {
    #[error("unknown event version")]
    UnknownVersion,
    #[error("a stream name is 1 to 256 bytes")]
    InvalidStream,
    #[error("the payload digest is not a sha256 digest")]
    InvalidDigest,
    #[error("a stream's sequences are dense: expected {expected}")]
    OutOfOrder { expected: u64 },
    #[error("a checkpoint never moves backwards")]
    CheckpointWentBackwards,
}

#[cfg(test)]
mod tests;
