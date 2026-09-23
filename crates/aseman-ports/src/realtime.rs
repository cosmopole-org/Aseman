//! Durable realtime ports (A707, ADR 0014).
//!
//! Publication and the application change that caused it share a transaction: an
//! event is recorded because the thing it describes happened, or neither did. That is
//! the outbox, and it is why the provider is a port and not a client of a broker.

use aseman_domain::Uuid;
use aseman_domain::realtime::{Checkpoint, Event};

use crate::PortResult;

/// One event with its payload, as a producer hands it over.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Publication {
    pub event: Event,
    /// The payload the digest covers.
    pub payload: Vec<u8>,
}

/// A batch a worker has claimed and is responsible for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Claim {
    pub events: Vec<Publication>,
    /// Until when this claim is the worker's.
    pub until_millis: i64,
}

/// The authoritative event log.
pub trait EventLog: Send + Sync {
    /// Append an event to its stream, in the same transaction as the change that
    /// caused it where the implementation allows.
    ///
    /// `Conflict` when the sequence is not the stream's next, so a producer that has
    /// raced learns it rather than leaving a gap.
    ///
    /// # Errors
    ///
    /// When the store refuses or is unreachable.
    fn append(&self, publication: &Publication) -> PortResult<()>;

    /// Events of `stream` after `sequence`, oldest first, at most `limit`.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn read(&self, stream: &str, after: u64, limit: usize) -> PortResult<Vec<Publication>>;

    /// The last sequence in `stream`, and the oldest it still holds.
    ///
    /// The oldest is what tells a consumer whether it may still replay or must
    /// resync.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn bounds(&self, stream: &str) -> PortResult<(Option<u64>, Option<u64>)>;

    /// Remove events past their retention class.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn purge_expired(&self, now_millis: i64) -> PortResult<u64>;
}

/// Where consumers have got to.
pub trait CheckpointStore: Send + Sync {
    /// # Errors
    ///
    /// When the store is unreachable.
    fn checkpoint(&self, consumer: &str, stream: &str) -> PortResult<Option<Checkpoint>>;

    /// Record a consumer's progress. `Conflict` when it would move backwards.
    ///
    /// # Errors
    ///
    /// When the store refuses or is unreachable.
    fn record(&self, checkpoint: &Checkpoint) -> PortResult<()>;
}

/// The transactional outbox: events waiting to be published onward.
///
/// Publication is singleton work and takes a fenced lease (ADR 0013); the fencing
/// token is recorded with the claim so a paused former holder cannot publish behind
/// the new one.
pub trait Outbox: Send + Sync {
    /// Claim at most `limit` unpublished events for `worker` until `until_millis`.
    ///
    /// A claim is skipped rather than waited on, so two workers never block each
    /// other — and, because only one holds the lease, never race either.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn claim(&self, worker: &str, limit: usize, until_millis: i64) -> PortResult<Claim>;

    /// Mark events published. Only the worker that claimed them may.
    ///
    /// # Errors
    ///
    /// When the store refuses or is unreachable.
    fn complete(&self, worker: &str, ids: &[Uuid]) -> PortResult<()>;

    /// Give a claim back without publishing, so another worker may take it.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn release(&self, worker: &str, ids: &[Uuid]) -> PortResult<()>;

    /// Events that have failed too many times, for an operator to look at.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn dead_letters(&self, limit: usize) -> PortResult<Vec<Publication>>;
}
