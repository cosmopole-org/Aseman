//! Federation ports (A704, A705).
//!
//! The home node is authoritative for its own descriptors. Everything else holds a
//! cache, and a cache is only ever allowed to move forward.

use aseman_domain::Uuid;
use aseman_domain::federation::{Envelope, NodeDescriptor, WorkloadDescriptor};

use crate::PortResult;

/// The descriptors this node knows: its own, and what it has learned about others.
pub trait Directory: Send + Sync {
    /// The node's own descriptor, as it publishes it.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn own_node(&self) -> PortResult<NodeDescriptor>;

    /// A node's descriptor, from the cache. `None` when it is unknown or expired.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn node(&self, node_id: Uuid, now_millis: i64) -> PortResult<Option<NodeDescriptor>>;

    /// Record a node descriptor. `Conflict` when its sequence does not move forward,
    /// so a replayed older descriptor cannot un-rotate a key.
    ///
    /// # Errors
    ///
    /// When the store refuses or is unreachable.
    fn record_node(&self, descriptor: &NodeDescriptor) -> PortResult<()>;

    /// A workload's minimal descriptor. `None` when it is unknown or expired.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn workload(
        &self,
        workload_id: Uuid,
        now_millis: i64,
    ) -> PortResult<Option<WorkloadDescriptor>>;

    /// Record a workload descriptor. `Conflict` when its revision does not move
    /// forward.
    ///
    /// # Errors
    ///
    /// When the store refuses or is unreachable.
    fn record_workload(&self, descriptor: &WorkloadDescriptor) -> PortResult<()>;
}

/// What a destination remembers so that a replay is refused and a retry is not
/// executed twice.
///
/// These are different things and are recorded separately on purpose: a **nonce** says
/// "this exact envelope has been seen", and a **request** says "this request has been
/// answered, here is the answer".
pub trait EnvelopeGuard: Send + Sync {
    /// Record `envelope`'s nonce, or report that it was already there.
    ///
    /// Returns `false` when the nonce had been seen, which is a replay.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn remember_nonce(&self, envelope: &Envelope) -> PortResult<bool>;

    /// The recorded answer to a request, when it has one. A retry is answered from
    /// here rather than executed again.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn recorded_answer(&self, request_id: Uuid) -> PortResult<Option<String>>;

    /// Record a request's answer.
    ///
    /// # Errors
    ///
    /// When the store refuses or is unreachable.
    fn record_answer(
        &self,
        request_id: Uuid,
        answer: &str,
        expires_at_millis: i64,
    ) -> PortResult<()>;

    /// Drop nonces and answers past their expiry.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn purge_expired(&self, now_millis: i64) -> PortResult<u64>;
}
