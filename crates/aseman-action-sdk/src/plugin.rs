//! The [`ActionPlugin`] trait — the full contract an action plugin implements.

use serde_json::Value;

use crate::context::ActionContext;
use crate::error::ActionError;
use crate::operation::ActionOperationSpec;

/// Static descriptor of an action plugin.
#[derive(Debug, Clone)]
pub struct ActionPluginMeta {
    /// The plugin's key (`aseman-action-creatures`), unique in the registry.
    pub key: &'static str,
    /// A human name for the plugin (`creatures`).
    pub name: &'static str,
    /// The operations it contributes.
    pub operations: Vec<ActionOperationSpec>,
}

/// A pluggable action implementation.
///
/// All methods speak JSON — the router's wire shape — so the interface stays
/// stable while individual families evolve. A plugin is registered at node
/// start-up by the aggregation crate; afterwards the router resolves every
/// operation through the registry, never naming an action in the node's own
/// code.
pub trait ActionPlugin: Send + Sync {
    /// Static descriptor of this plugin.
    fn meta(&self) -> &ActionPluginMeta;

    /// One-time hook invoked right after the plugin is registered.
    fn init(&self) {}

    /// Run the operation at `path` for `ctx` with the JSON `input`.
    ///
    /// # Errors
    ///
    /// [`ActionError::Invalid`] when the input is not the operation's, the
    /// refusal of the policy or of the handler, or a storage failure.
    fn run(
        &self,
        ctx: &dyn ActionContext,
        path: &str,
        input: &Value,
    ) -> Result<Value, ActionError>;
}

/// Convenience accessors over the trait.
impl dyn ActionPlugin {
    #[must_use]
    pub fn key(&self) -> &'static str {
        self.meta().key
    }

    #[must_use]
    pub fn name(&self) -> &'static str {
        self.meta().name
    }

    /// The operations this plugin contributes.
    #[must_use]
    pub fn operations(&self) -> &[ActionOperationSpec] {
        &self.meta().operations
    }
}