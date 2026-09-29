//! Core types for the `Core` compatibility orchestrator.
//!
//! Translation of `core/module/core/core.go`: the `Core` struct and its
//! supporting types. Cost and free-node state live in [`super::finance`].

use std::sync::{Arc, Mutex};

use aseman_config::AsemanConfig;
use rsa::RsaPrivateKey;

use crate::models::action::IActor;
use crate::models::globe::IGlobe;
use crate::models::ports::{
    INetwork, IRateLimiter, ISecurity, ISignaler, IStorage, ITools, IWorkloads,
};

/// Tools — aggregates every driver behind a single `ITools` impl.
pub struct Tools {
    pub(crate) security: Arc<dyn ISecurity>,
    pub(crate) signaler: Arc<dyn ISignaler>,
    pub(crate) storage: Arc<dyn IStorage>,
    pub(crate) network: Arc<dyn INetwork>,
    pub(crate) vmm: Arc<dyn IWorkloads>,
    pub(crate) rate_limiter: Arc<dyn IRateLimiter>,
}

impl ITools for Tools {
    fn security(&self) -> Arc<dyn ISecurity> {
        self.security.clone()
    }
    fn signaler(&self) -> Arc<dyn ISignaler> {
        self.signaler.clone()
    }
    fn storage(&self) -> Arc<dyn IStorage> {
        self.storage.clone()
    }
    fn network(&self) -> Arc<dyn INetwork> {
        self.network.clone()
    }
    fn workloads(&self) -> Arc<dyn IWorkloads> {
        self.vmm.clone()
    }
    fn rate_limiter(&self) -> Arc<dyn IRateLimiter> {
        self.rate_limiter.clone()
    }
}

/// The Caspar node orchestrator implementing [`ICore`].
pub struct Core {
    pub(crate) config: Option<Arc<AsemanConfig>>,
    pub(crate) owner_id: String,
    pub(crate) owner_priv_key: Arc<RsaPrivateKey>,
    pub(crate) id: String,
    pub(crate) ip: String,

    pub(crate) actor: Arc<dyn IActor>,
    pub(crate) tools: Mutex<Option<Arc<dyn ITools>>>,
    pub(crate) globe: Mutex<Option<Arc<dyn IGlobe>>>,
    #[expect(
        dead_code,
        reason = "RL-003: legacy orchestration surface kept until its deletion gate"
    )]
    pub(crate) started: Mutex<bool>,
    pub(crate) gods: Mutex<Vec<String>>,
    pub(crate) finance: super::finance::Finance,
    pub(crate) priv_key: Mutex<Option<Arc<RsaPrivateKey>>>,
}
