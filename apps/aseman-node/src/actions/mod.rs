//! The node's operations (ADR 0039): one router behind every surface.
//!
//! Each of the public contract's 76 operations is one handler here. The public
//! HTTP edge (A701), the signed-packet transports (TCP and WebSocket), the chain
//! and federation transports, and a guest's `execShellAction` all run the
//! same handler for the same operation, in one storage transaction that commits
//! when the handler succeeds and rolls back when it refuses (LD-15).
//!
//! An operation is addressed by its path (`/creatures/getByUsername`; the public
//! route is the same path under `/v1/actions`). Several operations may share one
//! A402 action, which is what they are authorized as; the table below is checked
//! against the registry when the router is built, so an operation cannot exist
//! without its action or a registered surface without its handler.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use aseman_contracts::security::PacketGuard;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::node::Node;
use crate::state::core_storage::StateFailure;
use crate::storage::Trx;

pub(crate) mod authority;
mod creature;
mod diagnostics;
pub(crate) mod dispatch;
mod file;
mod finance;
pub(crate) mod guard;
pub(crate) mod program;
pub(crate) mod secret;
mod store;
pub(crate) mod topic;
pub(crate) mod wire;
pub(crate) mod workload;

pub(crate) use creature::install_creature_types;
pub(crate) use workload::start_workload_services;

/// Who an operation runs for, as its transport established.
#[derive(Clone, Debug, Default)]
pub(crate) struct Caller {
    /// The acting creature, or empty for an anonymous caller.
    pub(crate) user_id: String,
    /// The store the guard admitted the caller to (store-guarded operations).
    pub(crate) store_id: String,
    /// The node the request came from: this node for a local request, the
    /// submitting node for one ordered on the chain.
    pub(crate) source: String,
}

/// Where a signed-packet request runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Origin {
    /// On the node that received it.
    Local,
    /// Ordered on the main chain and run by every node against its own state.
    Replicated,
    /// On the node the request's `origin` field names (this one when empty).
    Requested,
}

type Handler = fn(&Ctx<'_>, Value) -> Result<Value>;

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
/// signed-packet transports apply, and where a signed packet runs.
pub(crate) struct Operation {
    pub(crate) path: &'static str,
    pub(crate) action: String,
    pub(crate) guard: PacketGuard,
    pub(crate) origin: Origin,
    handler: Handler,
}

/// What a handler runs against: the node, the operation's transaction, and the
/// caller.
pub(crate) struct Ctx<'a> {
    pub(crate) node: &'a Arc<Node>,
    pub(crate) trx: &'a Trx,
    pub(crate) caller: &'a Caller,
}

/// The operation's input, strictly: a field of the wrong type is refused rather
/// than silently defaulted.
fn parse<T: DeserializeOwned>(input: Value) -> Result<T> {
    serde_json::from_value(input)
        .map_err(|error| InvalidInput(format!("invalid input: {error}")).into())
}

/// A body that is JSON but not the operation's input.
#[derive(Debug)]
struct InvalidInput(String);

impl std::fmt::Display for InvalidInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InvalidInput {}

macro_rules! operations {
    ($($path:literal $origin:ident => $handler:path;)*) => {
        &[$((
            $path,
            Origin::$origin,
            (|ctx: &Ctx<'_>, input: Value| $handler(ctx, parse(input)?)) as Handler,
        ),)*]
    };
}

const OPERATIONS: &[(&str, Origin, Handler)] = operations! {
    "/api/hello" Local => diagnostics::hello;
    "/api/ping" Local => diagnostics::ping;
    "/api/time" Local => diagnostics::time;
    "/auths/getServerPublicKey" Local => diagnostics::server_public_key;
    "/auths/getServersMap" Local => diagnostics::servers_map;

    "/creatures/create" Replicated => creature::create;
    "/creatures/get" Local => creature::get;
    "/creatures/getByUsername" Local => creature::get_by_username;
    "/creatures/find" Local => creature::find;
    "/creatures/list" Local => creature::list;
    "/machines/list" Local => creature::list_machines;
    "/creatures/meta" Replicated => creature::meta;
    "/creatures/update" Replicated => creature::update;
    "/creatures/delete" Replicated => creature::delete;
    "/creatures/types" Local => creature::types;
    "/creatures/signal" Local => creature::signal;
    "/creatures/authenticate" Local => creature::authenticate;
    "/creatures/checkSign" Local => creature::check_sign;

    "/creatures/secretPut" Replicated => secret::put;
    "/creatures/secretGet" Replicated => secret::get;
    "/creatures/secretList" Replicated => secret::list;
    "/creatures/secretListGranted" Replicated => secret::list_granted;
    "/creatures/secretGrant" Replicated => secret::grant;
    "/creatures/secretRevoke" Replicated => secret::revoke;
    "/storage/upload" Replicated => file::upload;

    "/creatures/transfer" Replicated => finance::transfer;
    "/creatures/mint" Replicated => finance::mint;
    "/creatures/lockToken" Replicated => finance::lock_token;
    "/creatures/consumeLock" Replicated => finance::consume_lock;
    "/creatures/getFinancialAccount" Replicated => finance::get_financial_account;
    "/creatures/paymentAdjustment" Replicated => finance::payment_adjustment;
    "/creatures/reconcileFinancialSystem" Replicated => finance::reconcile_financial_system;
    "/creatures/createHold" Replicated => finance::create_hold;
    "/creatures/getHold" Replicated => finance::get_hold;
    "/creatures/startHold" Replicated => finance::start_hold;
    "/creatures/releaseHold" Replicated => finance::release_hold;
    "/creatures/settleHold" Replicated => finance::settle_hold;
    "/creatures/openPool" Replicated => finance::open_pool;
    "/creatures/closePool" Replicated => finance::close_pool;
    "/creatures/debitPool" Replicated => finance::debit_pool;
    "/creatures/refreshPool" Replicated => finance::refresh_pool;
    "/creatures/reservePool" Replicated => finance::reserve_pool;
    "/creatures/releasePool" Replicated => finance::release_pool;
    "/creatures/settlePool" Replicated => finance::settle_pool;
    "/creatures/listPayouts" Replicated => finance::list_payouts;
    "/creatures/requestPayout" Replicated => finance::request_payout;
    "/creatures/resolvePayout" Replicated => finance::resolve_payout;
    "/creatures/publishFinanceCatalog" Replicated => finance::publish_finance_catalog;
    "/creatures/publishFinanceQuote" Replicated => finance::publish_finance_quote;
    "/creatures/registerFinanceNode" Replicated => finance::register_finance_node;
    "/creatures/retireFinanceNode" Replicated => finance::retire_finance_node;
    "/creatures/registerFinanceResource" Replicated => finance::register_finance_resource;
    "/creatures/retireFinanceResource" Replicated => finance::retire_finance_resource;
    "/creatures/reviewFinanceResource" Replicated => finance::review_finance_resource;

    "/stores/signal" Requested => store::signal;
    "/stores/history" Requested => store::history;
    "/stores/getAccess" Requested => store::get_access;
    "/stores/setAccess" Requested => store::set_access;

    "/gateway/signal" Local => topic::publish;
    "/gateway/subscribe" Local => topic::subscribe;
    "/gateway/unsubscribe" Local => topic::unsubscribe;

    "/programs/create" Replicated => program::create;
    "/programs/update" Replicated => program::update;
    "/programs/delete" Replicated => program::delete;
    "/programs/list" Local => program::list;
    "/machines/listProgramMachines" Local => program::list_program_machines;

    "/programs/deploy" Replicated => workload::deploy;
    "/programs/downloadEntity" Local => workload::download_entity;
    "/programs/deleteEntity" Local => workload::delete_entity;
    "/programs/runEntity" Local => workload::run_entity;
    "/programs/stopEntity" Local => workload::stop_entity;
    "/machines/listEntityVms" Local => workload::list_entity_vms;
    "/machines/readVmLogs" Replicated => workload::read_logs;
    "/machines/openVmTerminal" Replicated => workload::open_terminal;
    "/machines/closeVmTerminal" Replicated => workload::close_terminal;
    "/machines/readMachineBuilds" Replicated => workload::read_builds;
};

/// The route prefix of the public contract's operations.
const PUBLIC_ROUTE_PREFIX: &str = "/v1/actions";

/// Every operation, bound to the node it runs on.
pub(crate) struct Router {
    node: Arc<Node>,
    operations: BTreeMap<&'static str, Operation>,
}

impl Router {
    /// Bind the operation table to `node`, checked against the A402 registry.
    ///
    /// # Errors
    ///
    /// The registry does not load, or the table and the registry's shell surfaces
    /// disagree.
    pub(crate) fn new(node: Arc<Node>) -> Result<Arc<Self>> {
        let mut registered = aseman_contracts::security::shell_operations()
            .map_err(|error| anyhow!("the action registry: {error}"))?;
        let mut operations = BTreeMap::new();
        for &(path, origin, handler) in OPERATIONS {
            let entry = registered
                .remove(path)
                .ok_or_else(|| anyhow!("operation {path} is not a registered surface"))?;
            operations.insert(
                path,
                Operation {
                    path,
                    action: entry.action,
                    guard: entry.guard,
                    origin,
                    handler,
                },
            );
        }
        if let Some(path) = registered.keys().next() {
            return Err(anyhow!("registered surface {path} has no operation"));
        }
        Ok(Arc::new(Self { node, operations }))
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
                let ctx = Ctx {
                    node: &self.node,
                    trx,
                    caller,
                };
                (operation.handler)(&ctx, input)
            })
            .map_err(|failure| match failure {
                StateFailure::Action(error) => match error.downcast::<InvalidInput>() {
                    Ok(InvalidInput(message)) => OperationError::Invalid(message),
                    Err(error) => OperationError::Refused(error.to_string()),
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
