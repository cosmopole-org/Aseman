use std::sync::Arc;

use crate::models::ports::network::INetwork;
use crate::models::ports::ratelimit::IRateLimiter;
use crate::models::ports::security::ISecurity;
use crate::models::ports::signaler::ISignaler;
use crate::models::ports::storage::IStorage;
use crate::models::ports::workloads::IWorkloads;

/// Aggregates every node driver behind a single interface.
pub trait ITools: Send + Sync {
    fn security(&self) -> Arc<dyn ISecurity>;
    fn signaler(&self) -> Arc<dyn ISignaler>;
    fn storage(&self) -> Arc<dyn IStorage>;
    fn network(&self) -> Arc<dyn INetwork>;
    fn workloads(&self) -> Arc<dyn IWorkloads>;
    /// The shared, protocol-agnostic client-request rate limiter. Every
    /// client-facing transport consults this single instance so a client's
    /// quota is unified across TCP, WebSocket, and the HTTP ingress.
    fn rate_limiter(&self) -> Arc<dyn IRateLimiter>;
}
