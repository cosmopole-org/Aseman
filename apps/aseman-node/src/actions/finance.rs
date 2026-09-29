//! Finance: transfers and mints, token locks, holds, pools, payouts, and the
//! finance catalog. Each operation runs its use case (`aseman_application::finance`)
//! in the operation's transaction, with the finance ports over the storage module.
//! Value moves only on a real signature (the `finance` guard) except where noted.

use anyhow::{Result, anyhow};
use aseman_application::ApplicationError;
use aseman_application::finance as use_cases;
use chrono::Utc;
use serde_json::{Map, Value, json};

use super::Ctx;
use super::wire::creature::{ConsumeLockInput, LockTokenInput};
use crate::state::creature_ports::CreaturePorts;
use crate::state::finance_ports::FinanceLedgerPorts;
use crate::state::program_ports::ProgramPorts;
use crate::state::store_ports::{MembershipPorts, StorePorts, legacy_error};
use crate::state::token_locks;
use crate::util::crypto::secure_unique_string;
use crate::workloads::vmm::SystemClock;

type UseCase<A> = fn(&use_cases::FinancePorts<'_>, &str, A) -> Result<Value, ApplicationError>;

/// Run a finance use case for the caller in the operation's transaction.
fn run<A>(ctx: &Ctx<'_>, input: A, use_case: UseCase<A>) -> Result<Value> {
    let trx = ctx.trx;
    let ledger = FinanceLedgerPorts { trx };
    let creatures = CreaturePorts { trx };
    let stores = StorePorts { trx };
    let programs = ProgramPorts { trx };
    let membership = MembershipPorts { trx };
    let ports = use_cases::FinancePorts {
        ledger: &ledger,
        creatures: &creatures,
        balances: &creatures,
        stores: &stores,
        store_metadata: &stores,
        programs: &programs,
        access: &membership,
        clock: &SystemClock,
    };
    use_case(&ports, &ctx.caller.user_id, input).map_err(legacy_error)
}

macro_rules! finance_operations {
    ($($name:ident: $input:ident;)*) => {$(
        pub(super) fn $name(ctx: &Ctx<'_>, input: use_cases::$input) -> Result<Value> {
            run(ctx, input, use_cases::$name)
        }
    )*};
}

finance_operations! {
    transfer: TransferInput;
    mint: MintInput;
    get_financial_account: GetFinancialAccountInput;
    payment_adjustment: PaymentAdjustmentInput;
    reconcile_financial_system: ReconcileFinancialSystemInput;
    create_hold: CreateHoldInput;
    get_hold: GetHoldInput;
    start_hold: StartHoldInput;
    release_hold: ReleaseHoldInput;
    settle_hold: SettleHoldInput;
    open_pool: OpenPoolInput;
    close_pool: ClosePoolInput;
    debit_pool: DebitPoolInput;
    refresh_pool: RefreshPoolInput;
    reserve_pool: ReservePoolInput;
    release_pool: ReleasePoolInput;
    settle_pool: SettlePoolInput;
    list_payouts: ListPayoutsInput;
    request_payout: RequestPayoutInput;
    resolve_payout: ResolvePayoutInput;
    publish_finance_catalog: PublishFinanceCatalogInput;
    publish_finance_quote: PublishFinanceQuoteInput;
    register_finance_node: RegisterFinanceNodeInput;
    retire_finance_node: RetireFinanceNodeInput;
    register_finance_resource: RegisterFinanceResourceInput;
    retire_finance_resource: RetireFinanceResourceInput;
    review_finance_resource: ReviewFinanceResourceInput;
}

fn as_i64(raw: &Value) -> Option<i64> {
    match raw {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

/// Lock part of the caller's balance for a payee, as one amount or as a schedule
/// of steps, each consumable from its `unlockAt` on (`pay` locks).
pub(super) fn lock_token(ctx: &Ctx<'_>, input: LockTokenInput) -> Result<Value> {
    let creatures = CreaturePorts { trx: ctx.trx };
    let user_id = &ctx.caller.user_id;
    let mut user = creatures.account_or_empty(user_id)?;
    let mut steps = Vec::with_capacity(input.steps.len().max(1));
    if input.steps.is_empty() {
        if input.amount <= 0 {
            return Err(anyhow!("amount must be greater than zero"));
        }
        if input.unlock_at <= 0 {
            return Err(anyhow!("unlockAt must be a unix timestamp in milliseconds"));
        }
        steps.push(json!({"amount": input.amount, "unlockAt": input.unlock_at, "consumed": false}));
    } else {
        for (i, step) in input.steps.iter().enumerate() {
            if step.amount <= 0 {
                return Err(anyhow!("step {i} amount must be greater than zero"));
            }
            if step.unlock_at <= 0 {
                return Err(anyhow!(
                    "step {i} unlockAt must be a unix timestamp in milliseconds"
                ));
            }
            steps.push(
                json!({"amount": step.amount, "unlockAt": step.unlock_at, "consumed": false}),
            );
        }
    }
    let total_amount = steps.iter().try_fold(0_i64, |total, step| {
        total
            .checked_add(step["amount"].as_i64().unwrap_or(0))
            .ok_or_else(|| anyhow!("lock amount overflow"))
    })?;
    if user.balance < total_amount {
        return Err(anyhow!("your balance is not enough"));
    }
    if input.typ != "pay" {
        return Err(anyhow!("unknown lock type"));
    }
    if creatures.account(&input.target)?.is_none() {
        return Err(anyhow!("target user not acceptable"));
    }
    user.balance = user
        .balance
        .checked_sub(total_amount)
        .ok_or_else(|| anyhow!("balance underflow"))?;
    creatures.store_account(&user)?;
    let lock_id = secure_unique_string();
    let payload = json!({
        "type": "pay",
        "amount": total_amount,
        "remainingAmount": total_amount,
        "userId": input.target,
        "steps": steps,
    });
    token_locks::put_lock(
        ctx.trx,
        user_id,
        &lock_id,
        payload
            .as_object()
            .ok_or_else(|| anyhow!("invalid lock payload"))?,
        true,
    )?;
    Ok(json!({"tokenId": lock_id}))
}

/// Consume one step of a payer's lock for the caller, its payee, on the payer's
/// signature over `lockId:step:unlockAt:amount:payee`.
pub(super) fn consume_lock(ctx: &Ctx<'_>, input: ConsumeLockInput) -> Result<Value> {
    let creatures = CreaturePorts { trx: ctx.trx };
    let mut receiver = creatures.account_or_empty(&ctx.caller.user_id)?;
    if input.typ != "pay" {
        return Err(anyhow!("unknown lock type"));
    }
    if creatures.account(&input.user_id)?.is_none() {
        return Err(anyhow!("payer user not found"));
    }
    let sender = creatures.account_or_empty(&input.user_id)?;
    let Some(mut payment) = token_locks::lock(ctx.trx, &sender.id, &input.lock_id)? else {
        return Err(anyhow!("lock not found"));
    };
    let steps_raw = match payment.get("steps") {
        Some(Value::Array(steps)) if !steps.is_empty() => steps.clone(),
        _ => return Err(anyhow!("lock does not include steps")),
    };
    let now = Utc::now().timestamp_millis();
    let mut step_index = input.step.unwrap_or(-1);
    let mut steps: Vec<Map<String, Value>> = Vec::with_capacity(steps_raw.len());
    let mut amounts = Vec::with_capacity(steps_raw.len());
    let mut unlocks = Vec::with_capacity(steps_raw.len());
    for raw in &steps_raw {
        let Value::Object(step) = raw else {
            return Err(anyhow!("invalid lock step"));
        };
        let amount = step.get("amount").and_then(as_i64).unwrap_or(0);
        if amount <= 0 {
            return Err(anyhow!("invalid lock step amount"));
        }
        let unlock_at = step.get("unlockAt").and_then(as_i64).unwrap_or(0);
        if unlock_at <= 0 {
            return Err(anyhow!("invalid lock step unlockAt"));
        }
        let consumed = step
            .get("consumed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        steps.push(step.clone());
        amounts.push(amount);
        unlocks.push(unlock_at);
        if step_index == -1 && !consumed && now >= unlock_at && amount == input.amount {
            step_index = i64::try_from(steps.len() - 1)?;
        }
    }
    let Some(index) = usize::try_from(step_index)
        .ok()
        .filter(|i| *i < steps.len())
    else {
        return Err(anyhow!("lock step not found"));
    };
    if now < unlocks[index] {
        return Err(anyhow!("lock step is not consumable yet"));
    }
    if steps[index]
        .get("consumed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(anyhow!("lock step already consumed"));
    }
    if input.amount != amounts[index] {
        return Err(anyhow!("amount of payment not matched"));
    }
    let signed = format!(
        "{}:{}:{}:{}:{}",
        input.lock_id, index, unlocks[index], amounts[index], receiver.id
    );
    let (verified, _, _) = ctx.node.tools().security().auth_with_signature(
        &input.user_id,
        signed.as_bytes(),
        &input.signature,
    );
    if !verified {
        return Err(anyhow!("signature not verified"));
    }
    if payment.get("type").and_then(Value::as_str) != Some("pay") {
        return Err(anyhow!("type is not payment"));
    }
    if payment.get("userId").and_then(Value::as_str) != Some(receiver.id.as_str()) {
        return Err(anyhow!("you are not target"));
    }
    steps[index].insert("consumed".to_owned(), Value::Bool(true));
    steps[index].insert("consumedAt".to_owned(), json!(now));
    receiver.balance = receiver
        .balance
        .checked_add(input.amount)
        .ok_or_else(|| anyhow!("receiver balance overflow"))?;
    creatures.store_account(&receiver)?;
    let mut remaining = 0_i64;
    for (step, amount) in steps.iter().zip(&amounts) {
        if !step
            .get("consumed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            remaining = remaining
                .checked_add(*amount)
                .ok_or_else(|| anyhow!("remaining lock amount overflow"))?;
        }
    }
    if remaining == 0 {
        token_locks::delete_lock(ctx.trx, &sender.id, &input.lock_id)?;
    } else {
        let total = payment.get("amount").and_then(as_i64).unwrap_or(0);
        if total <= 0 {
            return Err(anyhow!("invalid lock total amount"));
        }
        payment.insert(
            "steps".to_owned(),
            Value::Array(steps.into_iter().map(Value::Object).collect()),
        );
        payment.insert("remainingAmount".to_owned(), json!(remaining));
        payment.insert("consumedAmount".to_owned(), json!(total - remaining));
        token_locks::put_lock(ctx.trx, &sender.id, &input.lock_id, &payment, true)?;
    }
    Ok(json!({"success": true, "step": index, "remainingAmount": remaining}))
}
