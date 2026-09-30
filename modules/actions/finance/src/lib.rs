//! Finance, as an action plugin (ADR 0040): transfers and mints, token locks,
//! holds, pools, payouts, and the finance catalog. Each operation runs its use
//! case (`aseman_application::finance`) in the operation's transaction, with the
//! finance ports over the storage module. Value moves only on a real signature
//! (the `finance` guard) except where noted.

use std::sync::Arc;

use serde_json::Value;
use aseman_action_sdk::{
    ActionContext, ActionError, ActionOperationSpec, ActionOrigin, ActionPlugin, ActionPluginMeta,
    action_error, parse,
};
use aseman_action_sdk::util::Ctx;

mod finance;

/// The finance operations.
pub struct FinancePlugin {
    meta: ActionPluginMeta,
}

impl FinancePlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            meta: ActionPluginMeta {
                key: "aseman-action-finance",
                name: "finance",
                operations: vec![
                    ActionOperationSpec::new("/creatures/transfer", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/mint", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/lockToken", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/consumeLock", ActionOrigin::Replicated),
                    ActionOperationSpec::new(
                        "/creatures/getFinancialAccount",
                        ActionOrigin::Replicated,
                    ),
                    ActionOperationSpec::new(
                        "/creatures/paymentAdjustment",
                        ActionOrigin::Replicated,
                    ),
                    ActionOperationSpec::new(
                        "/creatures/reconcileFinancialSystem",
                        ActionOrigin::Replicated,
                    ),
                    ActionOperationSpec::new("/creatures/createHold", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/getHold", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/startHold", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/releaseHold", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/settleHold", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/openPool", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/closePool", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/debitPool", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/refreshPool", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/reservePool", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/releasePool", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/settlePool", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/listPayouts", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/requestPayout", ActionOrigin::Replicated),
                    ActionOperationSpec::new("/creatures/resolvePayout", ActionOrigin::Replicated),
                    ActionOperationSpec::new(
                        "/creatures/publishFinanceCatalog",
                        ActionOrigin::Replicated,
                    ),
                    ActionOperationSpec::new(
                        "/creatures/publishFinanceQuote",
                        ActionOrigin::Replicated,
                    ),
                    ActionOperationSpec::new(
                        "/creatures/registerFinanceNode",
                        ActionOrigin::Replicated,
                    ),
                    ActionOperationSpec::new(
                        "/creatures/retireFinanceNode",
                        ActionOrigin::Replicated,
                    ),
                    ActionOperationSpec::new(
                        "/creatures/registerFinanceResource",
                        ActionOrigin::Replicated,
                    ),
                    ActionOperationSpec::new(
                        "/creatures/retireFinanceResource",
                        ActionOrigin::Replicated,
                    ),
                    ActionOperationSpec::new(
                        "/creatures/reviewFinanceResource",
                        ActionOrigin::Replicated,
                    ),
                ],
            },
        }
    }
}

impl Default for FinancePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionPlugin for FinancePlugin {
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
            "/creatures/transfer" => |c, i| finance::transfer(c, parse(&i)?),
            "/creatures/mint" => |c, i| finance::mint(c, parse(&i)?),
            "/creatures/lockToken" => |c, i| finance::lock_token(c, parse(&i)?),
            "/creatures/consumeLock" => |c, i| finance::consume_lock(c, parse(&i)?),
            "/creatures/getFinancialAccount" => |c, i| finance::get_financial_account(c, parse(&i)?),
            "/creatures/paymentAdjustment" => |c, i| finance::payment_adjustment(c, parse(&i)?),
            "/creatures/reconcileFinancialSystem" => {
                |c, i| finance::reconcile_financial_system(c, parse(&i)?)
            }
            "/creatures/createHold" => |c, i| finance::create_hold(c, parse(&i)?),
            "/creatures/getHold" => |c, i| finance::get_hold(c, parse(&i)?),
            "/creatures/startHold" => |c, i| finance::start_hold(c, parse(&i)?),
            "/creatures/releaseHold" => |c, i| finance::release_hold(c, parse(&i)?),
            "/creatures/settleHold" => |c, i| finance::settle_hold(c, parse(&i)?),
            "/creatures/openPool" => |c, i| finance::open_pool(c, parse(&i)?),
            "/creatures/closePool" => |c, i| finance::close_pool(c, parse(&i)?),
            "/creatures/debitPool" => |c, i| finance::debit_pool(c, parse(&i)?),
            "/creatures/refreshPool" => |c, i| finance::refresh_pool(c, parse(&i)?),
            "/creatures/reservePool" => |c, i| finance::reserve_pool(c, parse(&i)?),
            "/creatures/releasePool" => |c, i| finance::release_pool(c, parse(&i)?),
            "/creatures/settlePool" => |c, i| finance::settle_pool(c, parse(&i)?),
            "/creatures/listPayouts" => |c, i| finance::list_payouts(c, parse(&i)?),
            "/creatures/requestPayout" => |c, i| finance::request_payout(c, parse(&i)?),
            "/creatures/resolvePayout" => |c, i| finance::resolve_payout(c, parse(&i)?),
            "/creatures/publishFinanceCatalog" => {
                |c, i| finance::publish_finance_catalog(c, parse(&i)?)
            }
            "/creatures/publishFinanceQuote" => {
                |c, i| finance::publish_finance_quote(c, parse(&i)?)
            }
            "/creatures/registerFinanceNode" => {
                |c, i| finance::register_finance_node(c, parse(&i)?)
            }
            "/creatures/retireFinanceNode" => {
                |c, i| finance::retire_finance_node(c, parse(&i)?)
            }
            "/creatures/registerFinanceResource" => {
                |c, i| finance::register_finance_resource(c, parse(&i)?)
            }
            "/creatures/retireFinanceResource" => {
                |c, i| finance::retire_finance_resource(c, parse(&i)?)
            }
            "/creatures/reviewFinanceResource" => {
                |c, i| finance::review_finance_resource(c, parse(&i)?)
            }
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
    aseman_action_sdk::registry::register_plugin(Arc::new(FinancePlugin::new()));
}