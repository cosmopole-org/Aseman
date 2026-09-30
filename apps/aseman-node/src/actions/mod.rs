//! The node's operations (ADR 0039, ADR 0040): one router behind every surface.
//!
//! Each of the public contract's operations is contributed by an action plugin
//! (ADR 0040). The public HTTP edge (A701), the signed-packet transports (TCP
//! and WebSocket), the chain and federation transports, and a guest's
//! `execShellAction` all run the same handler for the same operation, in one
//! storage transaction that commits when the handler succeeds and rolls back
//! when it refuses (LD-15).
//!
//! An operation is addressed by its path (`/creatures/getByUsername`; the public
//! route is the same path under `/v1/actions`). The action it is authorized as
//! and its packet guard come from the A402 registry, which the router checks the
//! plugin table against when it is built: an operation cannot exist without its
//! action, a registered surface cannot exist without its plugin, and a path
//! cannot be claimed twice.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use aseman_action_sdk::registry;
use aseman_action_sdk::{ActionError, ActionOrigin, ActionPlugin, InvalidInput, NodeFacade};
use aseman_contracts::security::PacketGuard;
use serde_json::{Value, json};

use crate::node::Node;
use crate::state::core_storage::StateFailure;

pub(crate) mod authority;
pub(crate) mod dispatch;
pub(crate) mod guard;
pub(crate) mod host;
pub(crate) mod startup;

pub(crate) use aseman_action_sdk::wire;
pub(crate) use aseman_action_sdk::caller::ActionCaller as Caller;
pub(crate) use aseman_action_sdk::origin::ActionOrigin as Origin;

/// Why an operation did not take effect.
#[derive(Debug)]
pub(crate) enum OperationError {
    /// The body is not the operation's input.
    Invalid(String),
    /// The policy or the operation refused the request; its writes were
    /// discarded.
    Refused(String),
    /// The node's storage could not serve or commit it.
    Unavailable(String),
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Refused(message) | Self::Unavailable(message) => {
                formatter.write_str(message)
            }
        }
    }
}

/// One operation: its path, the action it is authorized as, the guard the
/// signed-packet transports apply, where a signed packet runs, and the plugin
/// that runs it.
pub(crate) struct Operation {
    pub(crate) path: &'static str,
    pub(crate) action: String,
    pub(crate) guard: PacketGuard,
    pub(crate) origin: ActionOrigin,
    plugin: Arc<dyn ActionPlugin>,
}

/// The route prefix of the public contract's operations.
const PUBLIC_ROUTE_PREFIX: &str = "/v1/actions";

/// Every registered operation, bound to the node it runs on and the plugin that
/// serves it.
pub(crate) struct Router {
    node: Arc<Node>,
    facade: Arc<NodeFacade>,
    operations: BTreeMap<&'static str, Operation>,
}

impl Router {
    /// Bind the registered plugins to `node`, checked against the A402 registry.
    ///
    /// # Errors
    ///
    /// The registry does not load, a plugin claims a path that is not a
    /// registered shell surface, a path is claimed twice, or the table and the
    /// registry's shell surfaces disagree.
    pub(crate) fn new(node: Arc<Node>) -> Result<Arc<Self>> {
        let mut registered = aseman_contracts::security::shell_operations()
            .map_err(|error| anyhow!("the action registry: {error}"))?;
        let facade = Arc::new(NodeFacade::new(Arc::new(host::NodeActionNode::new(
            node.clone(),
        ))));
        let mut operations = BTreeMap::new();
        for plugin in registry::plugins() {
            for spec in plugin.operations() {
                if operations.contains_key(spec.path) {
                    return Err(anyhow!(
                        "operation {} is claimed by two plugins",
                        spec.path
                    ));
                }
                let entry = registered.remove(spec.path).ok_or_else(|| {
                    anyhow!("operation {} is not a registered surface", spec.path)
                })?;
                operations.insert(
                    spec.path,
                    Operation {
                        path: spec.path,
                        action: entry.action,
                        guard: entry.guard,
                        origin: spec.origin,
                        plugin: plugin.clone(),
                    },
                );
            }
        }
        if let Some(path) = registered.keys().next() {
            return Err(anyhow!("registered surface {path} has no operation"));
        }
        Ok(Arc::new(Self {
            node,
            facade,
            operations,
        }))
    }

    pub(crate) fn node(&self) -> &Arc<Node> {
        &self.node
    }

    /// The operation at `route`: a path (`/creatures/get`) or its public route
    /// (`/v1/actions/creatures/get`).
    pub(crate) fn operation(&self, route: &str) -> Option<&Operation> {
        let path = route.strip_prefix(PUBLIC_ROUTE_PREFIX).unwrap_or(route);
        self.operations.get(path)
    }

    /// The operation an action runs as when only the action is known (a
    /// federated request): the first operation authorized as it.
    pub(crate) fn operation_for_action(&self, action: &str) -> Option<&Operation> {
        self.operations
            .values()
            .find(|operation| operation.action == action)
    }

    /// Run `operation` for `caller` with the JSON `body`, in one transaction.
    ///
    /// `authorize` runs the A402 decision for the signed-packet surfaces; the
    /// public edge has authorized the request before it gets here.
    ///
    /// # Errors
    ///
    /// Invalid input, the refusal of the policy or of the handler, or a failed
    /// commit.
    pub(crate) fn execute(
        &self,
        caller: &Caller,
        operation: &Operation,
        body: &[u8],
        authorize: bool,
    ) -> Result<Value, OperationError> {
        let input: Value = if body.iter().all(u8::is_ascii_whitespace) {
            json!({})
        } else {
            serde_json::from_slice(body)
                .map_err(|error| OperationError::Invalid(format!("invalid input: {error}")))?
        };
        let output = self
            .node
            .run(false, |trx| {
                if authorize {
                    authority::authorize_shell_action(
                        &authority::TrxLookups {
                            trx,
                            audit: self.node.audit(),
                        },
                        operation.path,
                        &caller.user_id,
                        &input,
                        chrono::Utc::now().timestamp_millis(),
                    )
                    .map_err(|refusal| anyhow!(refusal))?;
                }
                let ctx = host::NodeActionContext::new(&self.facade, trx, caller);
                operation
                    .plugin
                    .run(&ctx, operation.path, &input)
                    .map_err(anyhow::Error::new)
            })
            .map_err(|failure| match failure {
                StateFailure::Action(error) => match error.downcast::<ActionError>() {
                    Ok(ActionError::Invalid(message)) => OperationError::Invalid(message),
                    Ok(ActionError::Unavailable(message)) => OperationError::Unavailable(message),
                    Ok(ActionError::Refused(message)) => OperationError::Refused(message),
                    Err(error) => match error.downcast::<InvalidInput>() {
                        Ok(InvalidInput(message)) => OperationError::Invalid(message),
                        Err(error) => OperationError::Refused(error.to_string()),
                    },
                },
                StateFailure::Storage(error) => OperationError::Unavailable(error.to_string()),
            })?;
        if operation.action.starts_with("finance.") {
            self.order_finance_journal(&output);
        }
        Ok(output)
    }

    /// After a finance operation returns `{ "journalId": ... }`, offer the journal
    /// record for ordering through the consensus provider. The journal is
    /// already durable, so a failed offer does not fail the operation.
    fn order_finance_journal(&self, output: &Value) {
        let Some(consensus) = self.node.consensus_provider() else {
            return;
        };
        let Some(journal_id) = output
            .get("journalId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        let submit = aseman_application::consensus::SubmitFinanceRecord {
            consensus: &*consensus,
        };
        if let Err(error) = submit.execute(journal_id) {
            log::warn!("finance journal {journal_id} could not be offered for ordering: {error}");
        }
    }
}

#[cfg(test)]
mod tests;