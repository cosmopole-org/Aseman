//! Creature-owned secrets, as an action plugin (ADR 0040). A secret's value is
//! stored only as ciphertext under the node master key. The owner can always
//! read it back and may grant another creature time-boxed, revocable read
//! access.

use std::sync::Arc;

use serde_json::Value;
use aseman_action_sdk::{
    ActionContext, ActionError, ActionOperationSpec, ActionOrigin, ActionPlugin, ActionPluginMeta,
    action_error, parse,
};
use aseman_action_sdk::util::Ctx;

mod secret;

/// The creature-owned secret operations.
pub struct SecretsPlugin {
    meta: ActionPluginMeta,
}

impl SecretsPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            meta: ActionPluginMeta {
                key: "aseman-action-secrets",
                name: "secrets",
                operations: vec![
                    ActionOperationSpec::new("/creatures/secretPut", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/secretGet", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/secretList", ActionOrigin::Replicated),
                    ActionOperationSpec::new(
                        "/creatures/secretListGranted",
                        ActionOrigin::Replicated,
                    ),
                    ActionOperationSpec::new("/creatures/secretGrant", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/secretRevoke", ActionOrigin::Replicated),
                ],
            },
        }
    }
}

impl Default for SecretsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionPlugin for SecretsPlugin {
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
            "/creatures/secretPut" => |c, i| secret::put(c, parse(&i)?),
            "/creatures/secretGet" => |c, i| secret::get(c, parse(&i)?),
            "/creatures/secretList" => |c, i| secret::list(c, parse(&i)?),
            "/creatures/secretListGranted" => |c, i| secret::list_granted(c, parse(&i)?),
            "/creatures/secretGrant" => |c, i| secret::grant(c, parse(&i)?),
            "/creatures/secretRevoke" => |c, i| secret::revoke(c, parse(&i)?),
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
    aseman_action_sdk::registry::register_plugin(Arc::new(SecretsPlugin::new()));
}