//! Translation of `core/module/actor/actor.go`.

use std::sync::{Arc, Mutex};

use dashmap::DashMap;

use crate::models::action::actor::IActor;
use crate::models::action::{IAction, ISecureAction};
use crate::util::AnyVal;

/// Concrete [`IActor`] registry — stores actions by `key()` and lets callers
/// look them up at runtime. Secured actions live in a separate map so the
/// driver layer can resolve them as `Arc<dyn ISecureAction>` without
/// downcasting (which Rust doesn't support across distinct trait families).
/// `inject_service` is a no-op (matches Go), kept on the trait for parity.
pub struct Actor {
    actions: DashMap<String, Arc<dyn IAction>>,
    secure_actions: DashMap<String, Arc<dyn ISecureAction>>,
    services: Mutex<Vec<AnyVal>>,
}

impl Default for Actor {
    fn default() -> Self {
        Actor::new()
    }
}

impl Actor {
    /// Instantiate an empty `Actor`.
    pub fn new() -> Actor {
        Actor {
            actions: DashMap::new(),
            secure_actions: DashMap::new(),
            services: Mutex::new(Vec::new()),
        }
    }
}

impl IActor for Actor {
    fn inject_action(&self, action: Arc<dyn IAction>) {
        self.actions.insert(action.key(), action);
    }

    fn inject_service(&self, service: AnyVal) {
        // Go's implementation simply discarded the service; keep a copy so
        // tests can observe injection without changing externally-visible
        // semantics.
        self.services.lock().unwrap().push(service);
    }

    fn fetch_action(&self, key: &str) -> Option<Arc<dyn IAction>> {
        if let Some(a) = self.actions.get(key) {
            return Some(a.clone());
        }
        // Fall back to the inner action of a secure action. This lets
        // handlers that need to call another action directly (e.g.
        // /creatures/login calling /creatures/create) find it even though it
        // was only registered via inject_secure_action.
        self.secure_actions.get(key).map(|a| a.inner_action())
    }

    fn inject_secure_action(&self, action: Arc<dyn ISecureAction>) {
        self.secure_actions.insert(action.key(), action);
    }

    fn fetch_secure_action(&self, key: &str) -> Option<Arc<dyn ISecureAction>> {
        self.secure_actions.get(key).map(|a| a.clone())
    }
}
