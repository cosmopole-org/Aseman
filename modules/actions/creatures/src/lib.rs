//! Creatures, as an action plugin (ADR 0040): every being on the network that
//! can act (hold a balance, own resources, hold accesses). Their lifecycle,
//! lookups, types, identity, and direct signals.

use std::sync::Arc;

use serde_json::Value;
use aseman_action_sdk::{
    ActionContext, ActionError, ActionOperationSpec, ActionOrigin, ActionPlugin, ActionPluginMeta,
    action_error, parse,
};
use aseman_action_sdk::util::Ctx;

mod creature;

/// The creature lifecycle, lookup, identity, and signal operations.
pub struct CreaturesPlugin {
    meta: ActionPluginMeta,
}

impl CreaturesPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            meta: ActionPluginMeta {
                key: "aseman-action-creatures",
                name: "creatures",
                operations: vec![
                    ActionOperationSpec::new("/creatures/create", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/get", ActionOrigin::Local),
                    ActionOperationSpec::new("/creatures/getByUsername", ActionOrigin::Local),
                    ActionOperationSpec::new("/creatures/find", ActionOrigin::Local),
                    ActionOperationSpec::new("/creatures/list", ActionOrigin::Local),
                    ActionOperationSpec::new("/machines/list", ActionOrigin::Local),
                    ActionOperationSpec::new("/creatures/meta", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/update", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/delete", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/types", ActionOrigin::Local),
                    ActionOperationSpec::new("/creatures/signal", ActionOrigin::Local),
                    ActionOperationSpec::new("/creatures/authenticate", ActionOrigin::Local),
                    ActionOperationSpec::new("/creatures/checkSign", ActionOrigin::Local),
                ],
            },
        }
    }
}

impl Default for CreaturesPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionPlugin for CreaturesPlugin {
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
            "/creatures/create" => |c, i| creature::create(c, parse(&i)?),
            "/creatures/get" => |c, i| creature::get(c, parse(&i)?),
            "/creatures/getByUsername" => |c, i| creature::get_by_username(c, parse(&i)?),
            "/creatures/find" => |c, i| creature::find(c, parse(&i)?),
            "/creatures/list" => |c, i| creature::list(c, parse(&i)?),
            "/machines/list" => |c, i| creature::list_machines(c, parse(&i)?),
            "/creatures/meta" => |c, i| creature::meta(c, parse(&i)?),
            "/creatures/update" => |c, i| creature::update(c, parse(&i)?),
            "/creatures/delete" => |c, i| creature::delete(c, parse(&i)?),
            "/creatures/types" => |c, i| creature::types(c, parse(&i)?),
            "/creatures/signal" => |c, i| creature::signal(c, parse(&i)?),
            "/creatures/authenticate" => |c, i| creature::authenticate(c, parse(&i)?),
            "/creatures/checkSign" => |c, i| creature::check_sign(c, parse(&i)?),
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
    aseman_action_sdk::registry::register_plugin(Arc::new(CreaturesPlugin::new()));
}