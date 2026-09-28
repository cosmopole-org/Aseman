//! Core types for the `Core` compatibility orchestrator.
//!
//! Translation of `core/module/core/core.go`: the `Core` struct and its
//! supporting types (`Tools`, `ChainSubmission`, and the `WeakCoreView`
//! forwarding shim used by the transaction path). Cost and free-node state
//! live in [`super::finance`].

use std::sync::{Arc, Mutex};

use aseman_config::AsemanConfig;
use rsa::RsaPrivateKey;
use serde_json::Value;

use crate::models::action::IActor;
use crate::models::core::ICore;
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

/// Small forwarding shim around the parts of `Core` that an `Arc<dyn ICore>`
/// needs. `Core::weak_self` builds one of these on demand so the
/// `modify_state` family can synthesise an `Arc<dyn ICore>` for `TrxWrapper`
/// without holding a real reference to itself.
pub(crate) struct CoreWeakHandles {
    pub(crate) tools: Option<Arc<dyn ITools>>,
    pub(crate) actor: Arc<dyn IActor>,
    pub(crate) owner_id: String,
    pub(crate) id: String,
    pub(crate) ip: String,
    pub(crate) owner_priv_key: Arc<RsaPrivateKey>,
    pub(crate) priv_key: Option<Arc<RsaPrivateKey>>,
    pub(crate) finance: super::finance::Finance,
    pub(crate) globe: Option<Arc<dyn IGlobe>>,
    pub(crate) gods: Vec<String>,
}

pub(crate) struct WeakCoreView {
    pub(crate) inner: CoreWeakHandles,
}

impl WeakCoreView {
    /// A transaction over this view's storage, when the tools are loaded.
    pub(crate) fn checked_trx(
        &self,
        readonly: bool,
    ) -> Option<Arc<crate::adapters::rocksdb::trx::TrxWrapper>> {
        let tools = self.inner.tools.clone()?;
        let core_for_trx: Arc<dyn ICore> = Arc::new(WeakCoreView {
            inner: CoreWeakHandles {
                ..clone_handles(&self.inner)
            },
        });
        Some(crate::adapters::rocksdb::trx::TrxWrapper::new(
            core_for_trx,
            tools.storage(),
            readonly,
        ))
    }
}

pub(crate) fn clone_handles(h: &CoreWeakHandles) -> CoreWeakHandles {
    CoreWeakHandles {
        tools: h.tools.clone(),
        actor: h.actor.clone(),
        owner_id: h.owner_id.clone(),
        id: h.id.clone(),
        ip: h.ip.clone(),
        owner_priv_key: h.owner_priv_key.clone(),
        priv_key: h.priv_key.clone(),
        finance: h.finance.clone(),
        globe: h.globe.clone(),
        gods: h.gods.clone(),
    }
}

// `Value` import kept for signature parity with `ICore::load`.
const _: fn() -> Option<Value> = || None;
