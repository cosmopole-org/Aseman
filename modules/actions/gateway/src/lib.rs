//! Bridge topics (`/gateway/*`), as an action plugin (ADR 0040): the
//! subscription channel for programs that hold a socket open but are not
//! creatures.
//!
//! A creature's own VMs reach the node through their runtime's host calls.
//! Something running *beside* a VM — the crewAI bridge inside a Modal sandbox
//! — has neither a creature key nor a host ABI: it is an ordinary client
//! connection. It authenticates with a **bearer token its owning creature
//! minted** (`registerBridgeToken`), and that grant is the whole of its
//! authority: which topics it may subscribe to, and which creature its
//! signals are delivered to.
//!
//! The token is never stored: only its SHA-256, so a state dump does not hand
//! anybody a working credential. Actions here take the public guard —
//! anonymous, because a bridge cannot sign; the token *is* the identity
//! check, and every one of these bodies performs it before doing anything.

use std::sync::Arc;

use serde_json::Value;
use aseman_action_sdk::{
    ActionContext, ActionError, ActionOperationSpec, ActionOrigin, ActionPlugin, ActionPluginMeta,
    action_error, parse,
};
use aseman_action_sdk::util::Ctx;

mod topic;

/// The bridge topic operations.
pub struct GatewayPlugin {
    meta: ActionPluginMeta,
}

impl GatewayPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            meta: ActionPluginMeta {
                key: "aseman-action-gateway",
                name: "gateway",
                operations: vec![
                    ActionOperationSpec::new("/gateway/signal", ActionOrigin::Local),
                    ActionOperationSpec::new("/gateway/subscribe", ActionOrigin::Local),
                    ActionOperationSpec::new("/gateway/unsubscribe", ActionOrigin::Local),
                ],
            },
        }
    }
}

impl Default for GatewayPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionPlugin for GatewayPlugin {
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
            "/gateway/signal" => |c, i| topic::publish(c, parse(&i)?),
            "/gateway/subscribe" => |c, i| topic::subscribe(c, parse(&i)?),
            "/gateway/unsubscribe" => |c, i| topic::unsubscribe(c, parse(&i)?),
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
    aseman_action_sdk::registry::register_plugin(Arc::new(GatewayPlugin::new()));
}