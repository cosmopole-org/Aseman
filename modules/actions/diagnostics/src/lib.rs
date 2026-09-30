//! Node diagnostics and identity, as an action plugin (ADR 0040):
//! `/api/hello`, `/api/ping`, `/api/time`, and the node's public key and peers
//! (`node.diagnostics.read`, `node.identity.read`, `node.peers.read`).

use std::sync::Arc;

use serde_json::Value;
use aseman_action_sdk::{
    ActionContext, ActionError, ActionOperationSpec, ActionOrigin, ActionPlugin, ActionPluginMeta,
    action_error, parse,
};
use aseman_action_sdk::util::Ctx;

mod diagnostics;

/// The node diagnostics and identity operations.
pub struct DiagnosticsPlugin {
    meta: ActionPluginMeta,
}

impl DiagnosticsPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            meta: ActionPluginMeta {
                key: "aseman-action-diagnostics",
                name: "diagnostics",
                operations: vec![
                    ActionOperationSpec::new("/api/hello", ActionOrigin::Local),
                    ActionOperationSpec::new("/api/ping", ActionOrigin::Local),
                    ActionOperationSpec::new("/api/time", ActionOrigin::Local),
                    ActionOperationSpec::new("/auths/getServerPublicKey", ActionOrigin::Local),
                    ActionOperationSpec::new("/auths/getServersMap", ActionOrigin::Local),
                ],
            },
        }
    }
}

impl Default for DiagnosticsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionPlugin for DiagnosticsPlugin {
    fn meta(&self) -> &ActionPluginMeta {
        &self.meta
    }

    fn run(
        &self,
        ctx: &dyn ActionContext,
        path: &str,
        input: &Value,
    ) -> Result<Value, ActionError> {
        let ctx = Ctx::new(ctx);
        let dispatch: fn(&Ctx<'_>, Value) -> anyhow::Result<Value> = match path {
            "/api/hello" => |c, i| diagnostics::hello(c, parse(&i)?),
            "/api/ping" => |c, i| diagnostics::ping(c, parse(&i)?),
            "/api/time" => |c, i| diagnostics::time(c, parse(&i)?),
            "/auths/getServerPublicKey" => |c, i| diagnostics::server_public_key(c, parse(&i)?),
            "/auths/getServersMap" => |c, i| diagnostics::servers_map(c, parse(&i)?),
            _ => {
                return Err(ActionError::Refused(
                    "operation is not part of this plugin".to_owned(),
                ))
            }
        };
        dispatch(&ctx, input.clone()).map_err(action_error)
    }
}

/// Register this plugin with the action registry (called by the aggregation
/// crate at node start-up).
pub fn register() {
    aseman_action_sdk::registry::register_plugin(Arc::new(DiagnosticsPlugin::new()));
}