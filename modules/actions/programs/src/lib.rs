//! Programs, as an action plugin (ADR 0040): the code a machine runs. A program
//! belongs to a machine creature; only the owner of that machine may change or
//! delete it (LD-18).

use std::sync::Arc;

use serde_json::Value;
use aseman_action_sdk::{
    ActionContext, ActionError, ActionOperationSpec, ActionOrigin, ActionPlugin, ActionPluginMeta,
    action_error, parse,
};
use aseman_action_sdk::util::Ctx;

mod program;

/// The program lifecycle operations.
pub struct ProgramsPlugin {
    meta: ActionPluginMeta,
}

impl ProgramsPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            meta: ActionPluginMeta {
                key: "aseman-action-programs",
                name: "programs",
                operations: vec![
                    ActionOperationSpec::new("/programs/create", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/programs/update", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/programs/delete", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/programs/list", ActionOrigin::Local),
                    ActionOperationSpec::new(
                        "/machines/listProgramMachines",
                        ActionOrigin::Local,
                    ),
                ],
            },
        }
    }
}

impl Default for ProgramsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionPlugin for ProgramsPlugin {
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
            "/programs/create" => |c, i| program::create(c, parse(&i)?),
            "/programs/update" => |c, i| program::update(c, parse(&i)?),
            "/programs/delete" => |c, i| program::delete(c, parse(&i)?),
            "/programs/list" => |c, i| program::list(c, parse(&i)?),
            "/machines/listProgramMachines" => |c, i| program::list_program_machines(c, parse(&i)?),
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
    aseman_action_sdk::registry::register_plugin(Arc::new(ProgramsPlugin::new()));
}