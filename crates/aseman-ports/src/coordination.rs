//! The coordination port: fenced singleton leases (A607, ADR 0013).
//!
//! A replica takes a named lease before it does singleton work, and records the
//! lease's fencing token with every effect it commits. The port exists so that
//! "exactly one replica does this" is a property of the provider's transaction and
//! not of a process being alive.

use aseman_domain::coordination::{Acquisition, FencingToken, Lease, LeaseName};

use crate::PortResult;

/// Where a named lease lives.
///
/// Implementations must decide acquisition inside one transaction that locks the
/// lease row, must use the provider's own time rather than the caller's, and must
/// allocate a strictly increasing token on every acquisition. An advisory lock alone
/// does not satisfy this port: without a token there is nothing to fence a paused
/// holder out with.
pub trait CoordinationPort: Send + Sync {
    /// Take `name` for `instance` for `ttl_millis`, or report who holds it.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable or refuses the write.
    fn acquire(&self, name: &LeaseName, instance: &str, ttl_millis: i64)
    -> PortResult<Acquisition>;

    /// Extend a lease this instance holds, keeping its token.
    ///
    /// Returns the extended lease, or `None` when the lease was taken over or expired
    /// — in which case the holder must stop, not retry.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable or refuses the write.
    fn renew(&self, lease: &Lease, ttl_millis: i64) -> PortResult<Option<Lease>>;

    /// Give the lease up. Releasing a lease this instance no longer holds is a no-op:
    /// the new holder's claim is never disturbed by the old holder's cleanup.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable or refuses the write.
    fn release(&self, lease: &Lease) -> PortResult<()>;

    /// The lease as it stands, for an operator and for the destination-side guard.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable or refuses the read.
    fn read(&self, name: &LeaseName) -> PortResult<Option<Lease>>;

    /// The provider's current time in milliseconds. A holder decides whether to keep
    /// working from this, never from its own clock.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable.
    fn now_millis(&self) -> PortResult<i64>;
}

/// A destination that refuses an effect from a fenced-out holder (ADR 0013).
///
/// Used where an effect cannot be committed in the same transaction as the lease
/// check — an outbox publication, a directory publication, a call to another service.
/// The destination remembers the highest token it has accepted for a name and refuses
/// anything below it.
pub trait FencedDestination: Send + Sync {
    /// The highest token this destination has accepted under `name`.
    ///
    /// # Errors
    ///
    /// When the destination is unreachable.
    fn last_accepted(&self, name: &LeaseName) -> PortResult<Option<FencingToken>>;

    /// Record `token` as accepted, refusing (`Conflict`) anything below what is
    /// already recorded. This must be atomic with the effect it guards.
    ///
    /// # Errors
    ///
    /// [`crate::PortError::Conflict`] when a newer token has been accepted;
    /// otherwise when the destination is unreachable.
    fn accept(&self, name: &LeaseName, token: FencingToken) -> PortResult<()>;
}
