//! Stores, as an action plugin (ADR 0040): the unit a signal is addressed to.
//! A signal fans out live to every member that may read the store and, when the
//! store keeps history (`persHist`), is written to its log with the sender's
//! tags; `/stores/history` reads that log back filtered by tag. What a member
//! may *do* is checked here: `signal` to post, `read` to replay, `manage` to
//! change another member's grant (an absent grant denies). A store request whose
//! `origin` names another node runs there.

use std::sync::Arc;

use serde_json::Value;
use aseman_action_sdk::{
    ActionContext, ActionError, ActionOperationSpec, ActionOrigin, ActionPlugin, ActionPluginMeta,
    action_error, parse,
};
use aseman_action_sdk::util::Ctx;

mod store;

/// The store signal, history, and access operations.
pub struct StoresPlugin {
    meta: ActionPluginMeta,
}

impl StoresPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            meta: ActionPluginMeta {
                key: "aseman-action-stores",
                name: "stores",
                operations: vec![
                    ActionOperationSpec::new("/stores/signal", ActionOrigin::Requested),
                    ActionOperationSpec::new("/stores/history", ActionOrigin::Requested),
                    ActionOperationSpec::new("/stores/getAccess", ActionOrigin::Requested),
                    ActionOperationSpec::new("/stores/setAccess", ActionOrigin::Requested),
                ],
            },
        }
    }
}

impl Default for StoresPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionPlugin for StoresPlugin {
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
            "/stores/signal" => |c, i| store::signal(c, parse(&i)?),
            "/stores/history" => |c, i| store::history(c, parse(&i)?),
            "/stores/getAccess" => |c, i| store::get_access(c, parse(&i)?),
            "/stores/setAccess" => |c, i| store::set_access(c, parse(&i)?),
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
    aseman_action_sdk::registry::register_plugin(Arc::new(StoresPlugin::new()));
}