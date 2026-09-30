//! The global action plugin registry.
//!
//! Plugins are registered at node start-up by the aggregation crate; afterwards
//! the router resolves every operation through this registry — no action path is
//! ever named in the node's own code (ADR 0040).

use std::sync::{Arc, RwLock};

use crate::plugin::ActionPlugin;

static REGISTRY: RwLock<Vec<Arc<dyn ActionPlugin>>> = RwLock::new(Vec::new());

/// Register (or replace) a plugin. Called by each action project's `register()`
/// entry point via the aggregation crate.
pub fn register_plugin(plugin: Arc<dyn ActionPlugin>) {
    {
        let mut reg = REGISTRY.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = plugin.key().to_owned();
        reg.retain(|existing| existing.key() != key);
        reg.push(plugin.clone());
    }
    plugin.init();
}

/// Every registered plugin, in registration order.
pub fn plugins() -> Vec<Arc<dyn ActionPlugin>> {
    REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The canonical keys of every registered plugin.
pub fn keys() -> Vec<String> {
    REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|plugin| plugin.key().to_owned())
        .collect()
}