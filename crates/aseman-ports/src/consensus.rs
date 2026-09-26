//! The consensus port (Phase 8, RL-011).
//!
//! Finance never names a consensus implementation. It submits records for ordering and
//! reads what has been finalized; which service does the ordering — Hashgraph today,
//! something else tomorrow — is a composition decision.
//!
//! The core also does **not** drive provider-specific governance (staking thresholds,
//! election timing, validator caps, …) through a fixed API. Each provider is free to
//! decide which features it has and how it configures them. The port therefore exposes
//! only a generic, environment-style **config modifier** ([`ConsensusProvider::set`]):
//! the core writes provider-specific property/value pairs, and each provider interprets
//! the keys it understands. Providers that do not implement a key refuse it with
//! [`crate::PortError::Unsupported`], so a node can configure staking/election props
//! on a Hashgraph backend and the same composition works — without those features —
//! with a backend that does not have them.

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

    /// Set a provider-specific configuration property, environment-style.
    ///
    /// This is the one knob the core is allowed to turn on a consensus provider. Each
    /// provider documents and interprets its own keys — for example a Hashgraph backend
    /// understands staking thresholds and election timing, while a simpler backend may
    /// have no such properties at all. The core never assumes a provider has a feature;
    /// it writes the pair and the provider either applies it or refuses it.
    ///
    /// # Errors
    ///
    /// - [`crate::PortError::Unsupported`] when this provider has no such property.
    /// - [`crate::PortError::Failed`] when the value cannot be parsed.
    fn set(&self, key: &str, value: &str) -> PortResult<()>;
}
