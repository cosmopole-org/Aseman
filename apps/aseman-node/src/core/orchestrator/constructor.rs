//! `Core` construction: the `NewCore`/`NewCoreWithConfig` constructors and
//! the `mark_as_started` flag.
//!
//! Translation of `core/module/core/core.go`.

use std::sync::{Arc, Mutex};

use aseman_config::AsemanConfig;
use rsa::RsaPrivateKey;

use crate::core::actor::Actor;
use crate::core::orchestrator::finance::Finance;
use crate::core::orchestrator::types::Core;

impl Core {
    /// `NewCore(origin, ownerId, ownerPrivateKey)`.
    pub fn new(origin: &str, owner_id: &str, owner_priv_key: Arc<RsaPrivateKey>) -> Arc<Core> {
        Self::new_inner(origin, owner_id, owner_priv_key, None)
    }

    pub fn new_configured(
        origin: &str,
        owner_id: &str,
        owner_priv_key: Arc<RsaPrivateKey>,
        config: Arc<AsemanConfig>,
    ) -> Arc<Core> {
        Self::new_inner(origin, owner_id, owner_priv_key, Some(config))
    }

    fn new_inner(
        origin: &str,
        owner_id: &str,
        owner_priv_key: Arc<RsaPrivateKey>,
        config: Option<Arc<AsemanConfig>>,
    ) -> Arc<Core> {
        let finance = Finance::new(&config);
        Arc::new(Core {
            config,
            owner_id: owner_id.to_string(),
            owner_priv_key,
            id: origin.to_string(),
            ip: origin.to_string(),
            actor: Arc::new(Actor::new()),
            tools: Mutex::new(None),
            globe: Mutex::new(None),
            started: Mutex::new(false),
            gods: Mutex::new(Vec::new()),
            finance,
            priv_key: Mutex::new(None),
        })
    }

    pub fn mark_as_started(&self) {
        *self.started.lock().unwrap() = true;
    }
}
