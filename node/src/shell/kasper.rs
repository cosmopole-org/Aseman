//! Translation of `shell/kasper.go`.
//!
//! `Kasper` is a transparent alias for `Arc<dyn ICore>` (Go used `Kasper
//! core.ICore`). `new_app(...)` constructs a `Core` and returns the
//! concrete `Arc<Core>` cast to the trait so the rest of the shell can
//! work through the abstraction.

use std::sync::Arc;

use aseman_config::AsemanConfig;
use rsa::RsaPrivateKey;

use crate::core::core_orchestrator::Core;
use crate::models::core::ICore;

pub type Kasper = Arc<dyn ICore>;

/// Equivalent of Go's `NewApp(origin, ownerId, ownerPrivateKey)`.
pub fn new_app(origin: &str, owner_id: &str, owner_private_key: RsaPrivateKey) -> Arc<Core> {
    Core::new(origin, owner_id, Arc::new(owner_private_key))
}

pub fn new_configured_app(
    origin: &str,
    owner_id: &str,
    owner_private_key: RsaPrivateKey,
    config: Arc<AsemanConfig>,
) -> Arc<Core> {
    Core::new_configured(origin, owner_id, Arc::new(owner_private_key), config)
}
