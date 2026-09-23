//! Ordering and finalizing financial records (Phase 8, RL-011).
//!
//! Finance does not depend on a consensus implementation: wallets, pricing, and the
//! ledger are decided without one. What consensus adds is an **order** that several
//! nodes agree on, and a point past which that order will not change.
//!
//! The rule this module exists for: **a provider changes only at a finalized epoch.**
//! Swapping an ordering service mid-epoch would leave records ordered by one provider
//! and records ordered by another with nothing relating them — and money that cannot
//! be put in an order cannot be reconciled.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A finalized epoch. Monotonic: an epoch never reopens.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Epoch(u64);

impl Epoch {
    /// Before anything has been finalized.
    pub const GENESIS: Self = Self(0);

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// # Errors
    ///
    /// [`ConsensusError::EpochOverflow`] rather than wrapping into an epoch that has
    /// already been finalized.
    pub fn next(self) -> Result<Self, ConsensusError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(ConsensusError::EpochOverflow)
    }

    /// A stored epoch.
    #[must_use]
    pub const fn from_stored(value: u64) -> Self {
        Self(value)
    }
}

/// A financial record as consensus finalized it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finalized {
    /// The ledger idempotency key the record settles.
    pub idempotency_key: String,
    pub epoch: Epoch,
    /// Position within the epoch. Together with the epoch this is the total order.
    pub position: u64,
    /// `sha256:{hex}` of the record consensus agreed on, so a node can tell whether
    /// what it holds is what was finalized.
    pub digest: String,
}

/// Everything needed to hand ordering to a different provider.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    /// The last epoch the outgoing provider finalized.
    pub epoch: Epoch,
    /// How many records it finalized in total, up to and including that epoch.
    pub record_count: u64,
    /// `sha256:{hex}` over the finalized order, so the incoming provider can be shown
    /// to have inherited the same history.
    pub digest: String,
    pub taken_at_millis: i64,
}

/// Whether ordering may be handed from one provider to another.
///
/// # Errors
///
/// The reason it may not. Each one is a way the two providers' histories could fail to
/// join up.
pub fn may_switch(
    checkpoint: &Checkpoint,
    outgoing_epoch: Epoch,
    pending_records: u64,
) -> Result<(), ConsensusError> {
    if checkpoint.epoch != outgoing_epoch {
        // The checkpoint is of some other moment, so it does not describe what the
        // incoming provider would be inheriting.
        return Err(ConsensusError::StaleCheckpoint);
    }
    if pending_records > 0 {
        // Records submitted but not yet finalized would be ordered by neither
        // provider: the outgoing one has stopped and the incoming one never saw them.
        return Err(ConsensusError::RecordsInFlight(pending_records));
    }
    if !checkpoint
        .digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(ConsensusError::InvalidDigest);
    }
    Ok(())
}

/// Whether a newly received finalization may be accepted after what is already held.
///
/// The order only ever extends. A finalization that would reorder or replace what a
/// node has already acted on is refused, because the ledger entries behind it have
/// already been committed.
///
/// # Errors
///
/// The reason it may not be accepted.
pub fn accept_finalized(
    last: Option<&Finalized>,
    received: &Finalized,
) -> Result<(), ConsensusError> {
    let Some(last) = last else {
        return Ok(());
    };
    if received.epoch < last.epoch {
        return Err(ConsensusError::EpochWentBackwards);
    }
    if received.epoch == last.epoch && received.position <= last.position {
        return Err(ConsensusError::OutOfOrder);
    }
    Ok(())
}

/// Whether a node's own record matches what consensus finalized.
///
/// A mismatch is reported, never repaired automatically: two nodes disagreeing about a
/// financial record is a thing a person must look at.
#[must_use]
pub fn agrees(finalized: &Finalized, local_digest: &str) -> bool {
    finalized.digest == local_digest
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ConsensusError {
    #[error("the epoch counter is exhausted")]
    EpochOverflow,
    #[error("the checkpoint is not of the epoch being handed over")]
    StaleCheckpoint,
    #[error("{0} records are in flight and would be ordered by neither provider")]
    RecordsInFlight(u64),
    #[error("the checkpoint digest is not a sha256 digest")]
    InvalidDigest,
    #[error("a finalized epoch never reopens")]
    EpochWentBackwards,
    #[error("a finalized order only ever extends")]
    OutOfOrder,
}

#[cfg(test)]
mod tests;
