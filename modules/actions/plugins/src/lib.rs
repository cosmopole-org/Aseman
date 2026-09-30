//! Registration of every compiled action plugin (ADR 0040) — the aggregation
//! crate the node's composition root calls at the runtime phase, so the plugins
//! are statically compiled into the node binary while the node never names an
//! action. Mirrors the generated `vm-plugins` crate.

use std::sync::Once;

static REGISTER: Once = Once::new();

/// Register every action plugin compiled into this build. Idempotent — safe to
/// call from multiple init paths; the first call wins.
pub fn register_all() {
    REGISTER.call_once(|| {
        aseman_action_diagnostics::register();
        aseman_action_creatures::register();
        aseman_action_secrets::register();
        aseman_action_files::register();
        aseman_action_finance::register();
        aseman_action_stores::register();
        aseman_action_gateway::register();
        aseman_action_programs::register();
        aseman_action_workloads::register();
    });
}

/// The plugin keys compiled into this build.
pub fn action_plugin_keys() -> Vec<&'static str> {
    vec![
        "aseman-action-diagnostics",
        "aseman-action-creatures",
        "aseman-action-secrets",
        "aseman-action-files",
        "aseman-action-finance",
        "aseman-action-stores",
        "aseman-action-gateway",
        "aseman-action-programs",
        "aseman-action-workloads",
    ]
}