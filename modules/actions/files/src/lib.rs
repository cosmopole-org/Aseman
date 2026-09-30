//! Public files, as an action plugin (ADR 0040). The bytes live off-chain under
//! the node's public-files folder and only the returned id is meant to go
//! on-chain (an avatar id in a profile). The storage HTTP endpoint serves them
//! back at `GET /storage/file/<id>`; an owner sidecar records who uploaded each
//! one.

use std::sync::Arc;

use serde_json::Value;
use aseman_action_sdk::{
    ActionContext, ActionError, ActionOperationSpec, ActionOrigin, ActionPlugin, ActionPluginMeta,
    action_error, parse,
};
use aseman_action_sdk::util::Ctx;

mod file;

/// The public file upload operation.
pub struct FilesPlugin {
    meta: ActionPluginMeta,
}

impl FilesPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            meta: ActionPluginMeta {
                key: "aseman-action-files",
                name: "files",
                operations: vec![ActionOperationSpec::new(
                    "/storage/upload",
                    ActionOrigin::Replicated,
                )],
            },
        }
    }
}

impl Default for FilesPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionPlugin for FilesPlugin {
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
        match path {
            "/storage/upload" => {
                let input = parse(input).map_err(action_error)?;
                file::upload(&ctx, input).map_err(action_error)
            }
            _ => Err(ActionError::Refused(
                "operation is not part of this plugin".to_owned(),
            )),
        }
    }
}

/// Register this plugin with the action registry (called by the aggregation
/// crate at node start-up).
pub fn register() {
    aseman_action_sdk::registry::register_plugin(Arc::new(FilesPlugin::new()));
}