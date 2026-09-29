//! The node: its identity, configuration, prices, and the components it runs
//! (storage, security, the signaler, the network, workloads, the rate limiter, the
//! chain globe, and the VMM client).
//!
//! Components that need the node are built after it ([`Node::load`]), so they are
//! set once, after construction.

use std::sync::{Arc, OnceLock};

use aseman_config::AsemanConfig;
use rsa::RsaPrivateKey;

use crate::identity::Security;
use crate::live::hub::Signaler;
use crate::ratelimit::RateLimiter;
use crate::storage::NodeStorage;
use crate::transports::Network;
use crate::transports::chain::globe::Globe;
use crate::workloads::NodeWorkloads;
use crate::workloads::vmm::RemoteWorkloads;

/// The components the node runs.
pub struct Tools {
    pub(crate) security: Arc<Security>,
    pub(crate) signaler: Arc<Signaler>,
    pub(crate) storage: Arc<NodeStorage>,
    pub(crate) network: Arc<Network>,
    pub(crate) workloads: Arc<NodeWorkloads>,
    pub(crate) rate_limiter: Arc<RateLimiter>,
}

impl Tools {
    pub(crate) fn security(&self) -> Arc<Security> {
        self.security.clone()
    }
    pub(crate) fn signaler(&self) -> Arc<Signaler> {
        self.signaler.clone()
    }
    pub(crate) fn storage(&self) -> Arc<NodeStorage> {
        self.storage.clone()
    }
    pub(crate) fn network(&self) -> Arc<Network> {
        self.network.clone()
    }
    pub(crate) fn workloads(&self) -> Arc<NodeWorkloads> {
        self.workloads.clone()
    }
    pub(crate) fn rate_limiter(&self) -> Arc<RateLimiter> {
        self.rate_limiter.clone()
    }
}

/// A running node.
pub struct Node {
    pub(crate) config: Arc<AsemanConfig>,
    /// The node's id, which is also its origin.
    pub(crate) id: String,
    /// The creature that owns the node, and its key.
    pub(crate) owner_id: String,
    pub(crate) owner_key: Arc<RsaPrivateKey>,
    pub(crate) finance: super::finance::Finance,
    pub(crate) tools: OnceLock<Arc<Tools>>,
    pub(crate) globe: OnceLock<Arc<Globe>>,
    /// The node's own signing key (`server_key`), once its keys are loaded.
    pub(crate) node_key: OnceLock<Arc<RsaPrivateKey>>,
    /// The node's VMM, when one is configured.
    pub(crate) vmm: OnceLock<Arc<RemoteWorkloads>>,
    /// Where guest data is served, once the node's storage is open.
    pub(crate) guest_data: std::sync::OnceLock<crate::state::guest_data::GuestData>,
    /// The decision audit.
    pub(crate) audit: crate::state::audit::AuditLog,
    /// Bridge topic subscriptions.
    pub(crate) topics: crate::live::topics::Topics,
    /// The operations, which the chain, the transports, and guests run.
    pub(crate) router: OnceLock<Arc<crate::actions::Router>>,
}
