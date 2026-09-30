//! Entities and workloads, as an action plugin (ADR 0040): deploying a
//! program's entities, running them as VM instances on the node's VMM, and
//! reading their logs, terminals, and builds. Only the owner of a program's
//! machine may deploy, run, or stop it.
//!
//! A standalone instance on a node that charges for VMs is paid through a token
//! lock: its launch carries one signed lock step per minute, and the node's
//! billing sweep (a node-side startup service, not an operation) consumes a
//! step each minute through the chain.

use std::sync::Arc;

use serde_json::Value;
use aseman_action_sdk::{
    ActionContext, ActionError, ActionOperationSpec, ActionOrigin, ActionPlugin, ActionPluginMeta,
    action_error, parse,
};
use aseman_action_sdk::util::Ctx;

mod workload;

/// The entity and workload operations.
pub struct WorkloadsPlugin {
    meta: ActionPluginMeta,
}

impl WorkloadsPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            meta: ActionPluginMeta {
                key: "aseman-action-workloads",
                name: "workloads",
                operations: vec![
                    ActionOperationSpec::new("/programs/deploy", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/programs/downloadEntity", ActionOrigin::Local),
                    ActionOperationSpec::new("/programs/deleteEntity", ActionOrigin::Local),
                    ActionOperationSpec::new("/programs/runEntity", ActionOrigin::Local),
                    ActionOperationSpec::new("/programs/stopEntity", ActionOrigin::Local),
                    ActionOperationSpec::new("/machines/listEntityVms", ActionOrigin::Local),
                    ActionOperationSpec::new("/machines/readVmLogs", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/machines/openVmTerminal", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/machines/closeVmTerminal", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/machines/readMachineBuilds", ActionOrigin::Replicated),
                ],
            },
        }
    }
}

impl Default for WorkloadsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionPlugin for WorkloadsPlugin {
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
            "/programs/deploy" => |c, i| workload::deploy(c, parse(&i)?),
            "/programs/downloadEntity" => |c, i| workload::download_entity(c, parse(&i)?),
            "/programs/deleteEntity" => |c, i| workload::delete_entity(c, parse(&i)?),
            "/programs/runEntity" => |c, i| workload::run_entity(c, parse(&i)?),
            "/programs/stopEntity" => |c, i| workload::stop_entity(c, parse(&i)?),
            "/machines/listEntityVms" => |c, i| workload::list_entity_vms(c, parse(&i)?),
            "/machines/readVmLogs" => |c, i| workload::read_logs(c, parse(&i)?),
            "/machines/openVmTerminal" => |c, i| workload::open_terminal(c, parse(&i)?),
            "/machines/closeVmTerminal" => |c, i| workload::close_terminal(c, parse(&i)?),
            "/machines/readMachineBuilds" => |c, i| workload::read_builds(c, parse(&i)?),
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
    aseman_action_sdk::registry::register_plugin(Arc::new(WorkloadsPlugin::new()));
}