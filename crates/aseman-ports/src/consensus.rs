//! The consensus port (Phase 8, RL-011).
//!
//! Finance never names a consensus implementation. It submits records for ordering and
//! reads what has been finalized; which service does the ordering — Hashgraph today,
//! something else tomorrow — is a composition decision.

use aseman_domain::consensus::{Checkpoint, Epoch, Finalized};

use crate::PortResult;

/// An ordering service for financial records.
pub trait ConsensusProvider: Send + Sync {
    /// The provider's name, for the checkpoint that records a handover.
    fn name(&self) -> &str;

    /// Offer a record for ordering. Submitting the same idempotency key twice is
    /// success: the record is already in the order, or on its way into it.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable.
    fn submit(&self, idempotency_key: &str, digest: &str) -> PortResult<()>;

    /// The last epoch this provider has finalized.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable.
    fn finalized_epoch(&self) -> PortResult<Epoch>;

    /// Finalized records after `epoch`, in order, at most `limit`.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable.
    fn finalized_after(&self, epoch: Epoch, limit: usize) -> PortResult<Vec<Finalized>>;

    /// Records submitted but not yet finalized. A provider handover waits for this to
    /// be zero: anything in flight would be ordered by neither provider.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable.
    fn pending(&self) -> PortResult<u64>;

    /// Take a checkpoint of the finalized order, for handing it to another provider.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable.
    fn checkpoint(&self, at_millis: i64) -> PortResult<Checkpoint>;

    /// Adopt another provider's checkpoint as this provider's starting history.
    ///
    /// # Errors
    ///
    /// [`crate::PortError::Conflict`] when this provider has already finalized
    /// something of its own — adopting then would fork the order.
    fn adopt(&self, checkpoint: &Checkpoint) -> PortResult<()>;
}
