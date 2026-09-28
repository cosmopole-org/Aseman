//! The finance action family on the legacy packet paths (`/creatures/*Hold`, pools,
//! payouts, ...), as adapters over the finance use cases (`aseman_application::finance`)
//! that the public HTTP edge runs too (ADR 0036). Each action runs its use case in
//! the action's transaction, with the finance ports over the storage module.

use super::*;
use aseman_application::ApplicationError;
use aseman_application::finance as finance_use_cases;
use aseman_ports::finance_ledger::{FinanceLedger, WalletCounter};
use serde::de::DeserializeOwned;

use crate::api::model::finance_ports::FinanceLedgerPorts;
use crate::api::model::program_ports::ProgramPorts;
use crate::api::model::store_ports::{MembershipPorts, StorePorts, SystemClock};
use crate::core::trx::Trx;

type UseCase<A> =
    fn(&finance_use_cases::FinancePorts<'_>, &str, A) -> Result<Value, ApplicationError>;

fn port_error(error: aseman_ports::PortError) -> anyhow::Error {
    anyhow!("{error}")
}

pub(super) fn finance_debt_amount(trx: &Trx, user_id: &str) -> Result<i64> {
    FinanceLedgerPorts { trx }
        .counter(WalletCounter::Debt, user_id)
        .map_err(port_error)
}

pub(super) fn set_finance_debt_amount(trx: &Trx, user_id: &str, amount: i64) -> Result<()> {
    FinanceLedgerPorts { trx }
        .set_counter(WalletCounter::Debt, user_id, amount)
        .map_err(|_| anyhow!("wallet debt underflow"))
}

pub(super) fn finance_withdrawable_amount(trx: &Trx, user_id: &str) -> Result<i64> {
    FinanceLedgerPorts { trx }
        .counter(WalletCounter::Withdrawable, user_id)
        .map_err(port_error)
}

pub(super) fn set_finance_withdrawable_amount(
    trx: &Trx,
    user_id: &str,
    amount: i64,
) -> Result<()> {
    FinanceLedgerPorts { trx }
        .set_counter(WalletCounter::Withdrawable, user_id, amount)
        .map_err(|_| anyhow!("withdrawable balance underflow"))
}

pub(super) fn write_finance_journal(
    trx: &Trx,
    kind: &str,
    hold_id: &str,
    payer_id: &str,
    payload: Value,
    participants: &[String],
    now: i64,
) -> Result<String> {
    FinanceLedgerPorts { trx }
        .write_journal(kind, hold_id, payer_id, payload, participants, now)
        .map_err(port_error)
}

/// A finance action at `key`: its legacy input converted to the use case's input,
/// the use case run for the calling creature.
fn finance_action<I, A>(app: Arc<dyn ICore>, key: &str, use_case: UseCase<A>) -> Arc<dyn ISecureAction>
where
    I: crate::models::input::IInput
        + DeserializeOwned
        + serde::Serialize
        + Default
        + Clone
        + Send
        + Sync
        + 'static,
    A: DeserializeOwned + 'static,
{
    build_secure_action::<I, _>(app, key, finance_guard(), move |state, input: I| {
        let trx = state.trx();
        let caller = state.info().user_id();
        let input: A = serde_json::from_value(serde_json::to_value(&input)?)
            .map_err(|error| anyhow!("invalid finance input: {error}"))?;
        let ledger = FinanceLedgerPorts { trx: &trx };
        let creatures = crate::api::model::creature_ports::CreaturePorts { trx: &trx };
        let stores = StorePorts { trx: &trx };
        let programs = ProgramPorts { trx: &trx };
        let membership = MembershipPorts { trx: &trx };
        let ports = finance_use_cases::FinancePorts {
            ledger: &ledger,
            creatures: &creatures,
            balances: &creatures,
            stores: &stores,
            store_metadata: &stores,
            programs: &programs,
            access: &membership,
            clock: &SystemClock,
        };
        use_case(&ports, &caller, input).map_err(legacy_error)
    })
}

macro_rules! finance_actions {
    ($app:ident: $($key:literal => $input:ident, $use_case:ident;)*) => {
        vec![$(
            finance_action::<$input, finance_use_cases::$input>(
                $app.clone(),
                $key,
                finance_use_cases::$use_case,
            ),
        )*]
    };
}

pub(super) fn start_hold_handler(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    finance_action::<StartHoldInput, finance_use_cases::StartHoldInput>(
        app,
        "/creatures/startHold",
        finance_use_cases::start_hold,
    )
}

pub(super) fn handlers(app: Arc<dyn ICore>) -> Vec<Arc<dyn ISecureAction>> {
    finance_actions! { app:
        "/creatures/publishFinanceCatalog" => PublishFinanceCatalogInput, publish_finance_catalog;
        "/creatures/registerFinanceNode" => RegisterFinanceNodeInput, register_finance_node;
        "/creatures/retireFinanceNode" => RetireFinanceNodeInput, retire_finance_node;
        "/creatures/registerFinanceResource" => RegisterFinanceResourceInput, register_finance_resource;
        "/creatures/reviewFinanceResource" => ReviewFinanceResourceInput, review_finance_resource;
        "/creatures/retireFinanceResource" => RetireFinanceResourceInput, retire_finance_resource;
        "/creatures/publishFinanceQuote" => PublishFinanceQuoteInput, publish_finance_quote;
        "/creatures/createHold" => CreateHoldInput, create_hold;
        "/creatures/settleHold" => SettleHoldInput, settle_hold;
        "/creatures/releaseHold" => ReleaseHoldInput, release_hold;
        "/creatures/openPool" => OpenPoolInput, open_pool;
        "/creatures/refreshPool" => RefreshPoolInput, refresh_pool;
        "/creatures/closePool" => ClosePoolInput, close_pool;
        "/creatures/reservePool" => ReservePoolInput, reserve_pool;
        "/creatures/settlePool" => SettlePoolInput, settle_pool;
        "/creatures/releasePool" => ReleasePoolInput, release_pool;
        "/creatures/debitPool" => DebitPoolInput, debit_pool;
        "/creatures/getHold" => GetHoldInput, get_hold;
        "/creatures/getFinancialAccount" => GetFinancialAccountInput, get_financial_account;
        "/creatures/requestPayout" => RequestPayoutInput, request_payout;
        "/creatures/resolvePayout" => ResolvePayoutInput, resolve_payout;
        "/creatures/listPayouts" => ListPayoutsInput, list_payouts;
        "/creatures/reconcileFinancialSystem" => ReconcileFinancialSystemInput, reconcile_financial_system;
        "/creatures/paymentAdjustment" => PaymentAdjustmentInput, payment_adjustment;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A legacy finance input converts to its use case's input unchanged, so the
    /// signed hold request a quote records matches on both paths.
    #[test]
    fn legacy_finance_inputs_convert_to_use_case_inputs() {
        let legacy = CreateHoldInput {
            quote_id: "q1".into(),
            max_amount: 5,
            ..Default::default()
        };
        let converted: finance_use_cases::CreateHoldInput =
            serde_json::from_value(serde_json::to_value(&legacy).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&converted).unwrap(),
            serde_json::to_value(&legacy).unwrap()
        );
    }
}
