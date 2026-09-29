//! Translation of `shell/api/main/api.go`.
//!
//! Go used reflection to enumerate plugger methods at runtime; the Rust
//! port enumerates them explicitly: each action module's `install`
//! function registers its handlers with the actor.

use std::collections::HashMap;
use std::sync::Arc;

use crate::models::action::ExtendedField;
use crate::models::core::ICore;

use super::actions;

/// Mirrors `PlugAll(core, modelExtender)`; `advertised_port` is what `/api/ping`
/// reports.
pub fn plug_all(
    app: Arc<dyn ICore>,
    model_extender: &HashMap<String, HashMap<String, ExtendedField>>,
    advertised_port: &str,
) {
    actions::auth::install(app.clone());
    actions::creature::install(app.clone(), model_extender.clone());
    actions::dummy::install(app.clone(), advertised_port.to_owned());
    actions::gateway::install(app.clone());
    actions::program::install(app.clone());
    actions::store::install(app.clone());
}
