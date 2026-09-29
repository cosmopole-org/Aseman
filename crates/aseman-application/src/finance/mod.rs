//! Finance use cases.
//!
//! The finance family is a set of state machines over JSON documents and integer
//! counters. These use cases hold its rules and client-visible error texts, operating
//! over the [`FinanceLedger`] port plus the creature, store, program, and membership
//! ports, which the node supplies over its storage transaction.

use aseman_ports::finance_ledger::{FinanceDoc, FinanceLedger, FinanceMarker, WalletCounter};
use aseman_ports::{
    ClockPort, CreatureBalances, CreatureDirectory, PortError, ProgramDirectory, StoreAccess,
    StoreDirectory, StoreMetadata,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::ApplicationError;

fn denied(message: &str) -> ApplicationError {
    ApplicationError::Denied(message.to_owned())
}

fn failed(message: String) -> ApplicationError {
    ApplicationError::Port(PortError::Failed(message))
}

/// The root creature, the operator of the finance surfaces.
pub const LEGACY_ROOT: &str = "1@global";

const FINANCE_HOLD_MAX_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const FINANCE_MAX_BENEFICIARIES: usize = 64;
const FEDERATED_FINANCE_MAX_RECORD_BYTES: usize = 256 * 1024;

fn valid_finance_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@'))
}

fn valid_finance_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_finance_origin(value: &str) -> bool {
    if valid_finance_id(value) {
        return true;
    }
    if value.is_empty()
        || value.len() > 2_048
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return false;
    }
    let Ok(origin) = url::Url::parse(value) else {
        return false;
    };
    matches!(origin.scheme(), "http" | "https" | "ws" | "wss")
        && origin.host_str().is_some()
        && origin.username().is_empty()
        && origin.password().is_none()
        && matches!(origin.path(), "" | "/")
        && origin.query().is_none()
        && origin.fragment().is_none()
}

fn finance_hash(value: &Value) -> Result<String, ApplicationError> {
    let bytes = serde_json::to_vec(value).map_err(|error| failed(error.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn as_i64(raw: &Value) -> Option<i64> {
    match raw {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

fn finance_map_add(
    totals: &mut std::collections::HashMap<String, i64>,
    key: &str,
    amount: i64,
) -> bool {
    if key.is_empty() || amount < 0 {
        return false;
    }
    let current = totals.get(key).copied().unwrap_or(0);
    let Some(next) = current.checked_add(amount) else {
        return false;
    };
    totals.insert(key.to_string(), next);
    true
}

/// The portion of `amount` a wallet must draw from its withdrawable balance.
fn withdrawable_debit_portion(balance: i64, withdrawable: i64, amount: i64) -> i64 {
    amount.saturating_sub(balance.saturating_sub(withdrawable))
}

/// A creature's financial account as finance code reads and writes it.
#[derive(Clone, Debug)]
pub struct Account {
    pub id: String,
    pub balance: i64,
}

/// The finance ports of one state action.
pub struct FinancePorts<'a> {
    pub ledger: &'a dyn FinanceLedger,
    pub creatures: &'a dyn CreatureDirectory,
    pub balances: &'a dyn CreatureBalances,
    pub stores: &'a dyn StoreDirectory,
    pub store_metadata: &'a dyn StoreMetadata,
    pub programs: &'a dyn ProgramDirectory,
    pub access: &'a dyn StoreAccess,
    pub clock: &'a dyn ClockPort,
}

impl FinancePorts<'_> {
    /// The creature's balance account, or `None` when the creature is absent (LD-13).
    fn account(&self, creature_id: &str) -> Result<Option<Account>, ApplicationError> {
        match self.balances.balance(creature_id) {
            Ok(balance) => Ok(Some(Account {
                id: creature_id.to_owned(),
                balance,
            })),
            Err(PortError::NotFound) => Ok(None),
            Err(error) => Err(ApplicationError::Port(error)),
        }
    }

    fn store_account(&self, account: &Account) -> Result<(), ApplicationError> {
        self.balances
            .set_balance(&account.id, account.balance)
            .map_err(ApplicationError::from)
    }

    /// Whether `user_id` holds a store membership (`has_access_to_store`).
    fn is_project_member(&self, user_id: &str, store_id: &str) -> Result<bool, ApplicationError> {
        self.access
            .is_member(store_id, user_id)
            .map_err(ApplicationError::from)
    }

    /// The metadata object at `path`, as `get_json(..).ok()` returned it.
    fn store_metadata_object(&self, store_id: &str, path: &str) -> Option<Map<String, Value>> {
        let text = self
            .store_metadata
            .store_metadata(store_id, path)
            .ok()
            .flatten()?;
        serde_json::from_str(&text).ok()
    }
}

// ── Wallet counters ───────────────────────────────────────────────────────────

fn finance_counter(
    ledger: &dyn FinanceLedger,
    kind: WalletCounter,
    user: &str,
) -> Result<i64, ApplicationError> {
    ledger.counter(kind, user).map_err(ApplicationError::from)
}

fn set_finance_counter(
    ledger: &dyn FinanceLedger,
    kind: WalletCounter,
    user: &str,
    amount: i64,
) -> Result<(), ApplicationError> {
    ledger
        .set_counter(kind, user, amount)
        .map_err(ApplicationError::from)
}

fn add_finance_counter(
    ledger: &dyn FinanceLedger,
    kind: WalletCounter,
    user: &str,
    amount: i64,
) -> Result<i64, ApplicationError> {
    ledger
        .add_counter(kind, user, amount)
        .map_err(ApplicationError::from)
}

// ── Document accessors ────────────────────────────────────────────────────────

fn get_finance_hold(
    ledger: &dyn FinanceLedger,
    hold_id: &str,
) -> Result<Map<String, Value>, ApplicationError> {
    ledger
        .get_doc(FinanceDoc::Hold, hold_id, "hold")
        .map_err(|error| match error {
            PortError::NotFound => denied("hold not found"),
            other => ApplicationError::Port(other),
        })
}

fn put_finance_hold(
    ledger: &dyn FinanceLedger,
    hold_id: &str,
    hold: &Map<String, Value>,
) -> Result<(), ApplicationError> {
    ledger
        .put_doc(
            FinanceDoc::Hold,
            hold_id,
            "hold",
            &Value::Object(hold.clone()),
            false,
        )
        .map_err(ApplicationError::from)
}

fn get_finance_pool(
    ledger: &dyn FinanceLedger,
    pool_id: &str,
) -> Result<Map<String, Value>, ApplicationError> {
    ledger
        .get_doc(FinanceDoc::Pool, pool_id, "pool")
        .map_err(|error| match error {
            PortError::NotFound => denied("pool not found"),
            other => ApplicationError::Port(other),
        })
}

fn put_finance_pool(
    ledger: &dyn FinanceLedger,
    pool_id: &str,
    pool: &Map<String, Value>,
) -> Result<(), ApplicationError> {
    ledger
        .put_doc(
            FinanceDoc::Pool,
            pool_id,
            "pool",
            &Value::Object(pool.clone()),
            false,
        )
        .map_err(ApplicationError::from)
}

fn get_finance_pool_reservation(
    ledger: &dyn FinanceLedger,
    run_id: &str,
) -> Result<Map<String, Value>, ApplicationError> {
    ledger
        .get_doc(FinanceDoc::PoolReservation, run_id, "reservation")
        .map_err(|error| match error {
            PortError::NotFound => denied("run reservation not found"),
            other => ApplicationError::Port(other),
        })
}

fn put_finance_pool_reservation(
    ledger: &dyn FinanceLedger,
    run_id: &str,
    reservation: &Map<String, Value>,
) -> Result<(), ApplicationError> {
    ledger
        .put_doc(
            FinanceDoc::PoolReservation,
            run_id,
            "reservation",
            &Value::Object(reservation.clone()),
            false,
        )
        .map_err(ApplicationError::from)
}

fn put_finance_live_debit(
    ledger: &dyn FinanceLedger,
    run_id: &str,
    record: &Map<String, Value>,
) -> Result<(), ApplicationError> {
    ledger
        .put_doc(
            FinanceDoc::LiveDebit,
            run_id,
            "debit",
            &Value::Object(record.clone()),
            false,
        )
        .map_err(ApplicationError::from)
}

fn get_finance_payout(
    ledger: &dyn FinanceLedger,
    payout_id: &str,
) -> Result<Map<String, Value>, ApplicationError> {
    ledger
        .get_doc(FinanceDoc::Payout, payout_id, "payout")
        .map_err(|error| match error {
            PortError::NotFound => denied("payout not found"),
            other => ApplicationError::Port(other),
        })
}

fn put_finance_payout(
    ledger: &dyn FinanceLedger,
    payout_id: &str,
    payout: &Map<String, Value>,
) -> Result<(), ApplicationError> {
    ledger
        .put_doc(
            FinanceDoc::Payout,
            payout_id,
            "payout",
            &Value::Object(payout.clone()),
            false,
        )
        .map_err(ApplicationError::from)
}

// ── Journal ───────────────────────────────────────────────────────────────────

fn write_finance_journal(
    ledger: &dyn FinanceLedger,
    kind: &str,
    hold_id: &str,
    payer_id: &str,
    payload: Value,
    participants: &[String],
    now: i64,
) -> Result<String, ApplicationError> {
    ledger
        .write_journal(kind, hold_id, payer_id, payload, participants, now)
        .map_err(ApplicationError::from)
}

// ── Project budgets ───────────────────────────────────────────────────────────

fn finance_nonnegative_field(
    map: &Map<String, Value>,
    field: &str,
) -> Result<i64, ApplicationError> {
    let Some(value) = map.get(field) else {
        return Ok(0);
    };
    let amount =
        as_i64(value).ok_or_else(|| denied(&format!("invalid project budget field: {field}")))?;
    if amount < 0 {
        return Err(denied(&format!("invalid project budget field: {field}")));
    }
    Ok(amount)
}

fn reserve_project_budget(
    ports: &FinancePorts,
    project_id: &str,
    amount: i64,
    now: i64,
) -> Result<(), ApplicationError> {
    if project_id.is_empty() {
        return Ok(());
    }
    if ports.stores.store(project_id)?.is_none() {
        return Err(denied("project not found"));
    }
    let metadata = ports
        .store_metadata_object(project_id, "metadata")
        .unwrap_or_default();
    let configured_budget = finance_nonnegative_field(&metadata, "budgetMinor")?;
    let metadata_spent = finance_nonnegative_field(&metadata, "spentMinor")?;
    let mut state = ports
        .ledger
        .get_doc(FinanceDoc::ProjectBudget, project_id, "budget")
        .unwrap_or_default();
    let ledger_spent = finance_nonnegative_field(&state, "spentMinor")?;
    let spent = ledger_spent.max(metadata_spent);
    let reserved = finance_nonnegative_field(&state, "reservedMinor")?;
    let committed = spent
        .checked_add(reserved)
        .and_then(|value| value.checked_add(amount))
        .ok_or_else(|| denied("project budget overflow"))?;
    if configured_budget > 0 && committed > configured_budget {
        return Err(denied("project budget exceeded"));
    }
    state.insert("projectId".to_string(), json!(project_id));
    state.insert("budgetMinor".to_string(), json!(configured_budget));
    state.insert("spentMinor".to_string(), json!(spent));
    state.insert(
        "reservedMinor".to_string(),
        json!(
            reserved
                .checked_add(amount)
                .ok_or_else(|| denied("project reservation overflow"))?
        ),
    );
    state.insert("updatedAt".to_string(), json!(now));
    ports
        .ledger
        .put_doc(
            FinanceDoc::ProjectBudget,
            project_id,
            "budget",
            &Value::Object(state),
            false,
        )
        .map_err(ApplicationError::from)
}

fn finalize_project_budget(
    ports: &FinancePorts,
    project_id: &str,
    reserved_amount: i64,
    spent_amount: i64,
    now: i64,
) -> Result<(), ApplicationError> {
    if project_id.is_empty() {
        return Ok(());
    }
    let mut state = ports
        .ledger
        .get_doc(FinanceDoc::ProjectBudget, project_id, "budget")
        .map_err(|error| match error {
            PortError::NotFound => denied("project budget reservation not found"),
            other => ApplicationError::Port(other),
        })?;
    let reserved = finance_nonnegative_field(&state, "reservedMinor")?;
    let spent = finance_nonnegative_field(&state, "spentMinor")?;
    let remaining = reserved
        .checked_sub(reserved_amount)
        .ok_or_else(|| denied("project budget reservation underflow"))?;
    let total_spent = spent
        .checked_add(spent_amount)
        .ok_or_else(|| denied("project spend overflow"))?;
    state.insert("reservedMinor".to_string(), json!(remaining));
    state.insert("spentMinor".to_string(), json!(total_spent));
    state.insert("updatedAt".to_string(), json!(now));
    ports
        .ledger
        .put_doc(
            FinanceDoc::ProjectBudget,
            project_id,
            "budget",
            &Value::Object(state),
            false,
        )
        .map_err(ApplicationError::from)
}

// ── Account snapshot ──────────────────────────────────────────────────────────

fn finance_payout_records(
    ports: &FinancePorts,
    user_id: &str,
    limit: usize,
) -> Result<Vec<Value>, ApplicationError> {
    let mut keys = ports.ledger.payout_ids_by_user(user_id, limit)?;
    keys.sort();
    keys.reverse();
    let mut payouts = Vec::new();
    for payout_id in keys.into_iter().take(limit) {
        if let Ok(payout) = get_finance_payout(ports.ledger, &payout_id) {
            payouts.push(Value::Object(payout));
        }
    }
    Ok(payouts)
}

fn financial_account_snapshot(
    ports: &FinancePorts,
    user_id: &str,
    limit: usize,
) -> Result<Value, ApplicationError> {
    let Some(creature) = ports.account(user_id)? else {
        return Err(denied("financial account not found"));
    };
    let mut journal_keys = ports.ledger.journal_ids_by_user(user_id, limit)?;
    journal_keys.sort();
    journal_keys.reverse();
    let mut transactions = Vec::new();
    for journal_id in journal_keys.into_iter().take(limit) {
        if let Ok(entry) = ports
            .ledger
            .get_doc(FinanceDoc::Journal, &journal_id, "entry")
        {
            transactions.push(Value::Object(entry));
        }
    }
    let mut hold_keys = ports.ledger.hold_ids_by_payer(user_id, 100)?;
    hold_keys.sort();
    hold_keys.reverse();
    let mut active_holds = Vec::new();
    for hold_id in hold_keys.into_iter().take(100) {
        if let Ok(hold) = get_finance_hold(ports.ledger, &hold_id) {
            let status = hold.get("status").and_then(Value::as_str).unwrap_or("");
            if status == "open" || status == "running" {
                active_holds.push(Value::Object(hold));
            }
        }
    }
    let pool = {
        let pool_id = ports.ledger.pool_of_user(user_id)?;
        if pool_id.is_empty() {
            Value::Null
        } else {
            match ports.ledger.get_doc(FinanceDoc::Pool, &pool_id, "pool") {
                Ok(p) if p.get("status").and_then(Value::as_str) == Some("open") => {
                    Value::Object(p)
                }
                _ => Value::Null,
            }
        }
    };
    Ok(json!({
        "userId": user_id,
        "availableMinor": creature.balance,
        "heldMinor": finance_counter(ports.ledger, WalletCounter::Held, user_id)?,
        "debtMinor": finance_counter(ports.ledger, WalletCounter::Debt, user_id)?,
        "withdrawableMinor": finance_counter(ports.ledger, WalletCounter::Withdrawable, user_id)?,
        "payoutHeldMinor": finance_counter(ports.ledger, WalletCounter::PayoutHeld, user_id)?,
        "earnedMinor": finance_counter(ports.ledger, WalletCounter::Earned, user_id)?,
        "spentMinor": finance_counter(ports.ledger, WalletCounter::Spent, user_id)?,
        "activeHolds": active_holds,
        "pool": pool,
        "transactions": transactions,
        "payouts": finance_payout_records(ports, user_id, limit)?,
    }))
}

// ── Market documents ──────────────────────────────────────────────────────────

fn federated_finance_object(
    value: Value,
    label: &str,
) -> Result<Map<String, Value>, ApplicationError> {
    let object = value
        .as_object()
        .cloned()
        .ok_or_else(|| denied(&format!("{label} must be an object")))?;
    if serde_json::to_vec(&object)
        .map_err(|error| failed(error.to_string()))?
        .len()
        > FEDERATED_FINANCE_MAX_RECORD_BYTES
    {
        return Err(denied(&format!("{label} is too large")));
    }
    Ok(object)
}

fn federated_finance_safe_numbers(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => true,
        Value::Number(number) => number
            .as_i64()
            .map(|n| (0..=9_007_199_254_740_991).contains(&n))
            .unwrap_or(false),
        Value::Array(values) => values.iter().all(federated_finance_safe_numbers),
        Value::Object(values) => values.values().all(federated_finance_safe_numbers),
    }
}

fn federated_finance_market_bucket(kind: &str) -> Option<&'static str> {
    match kind {
        "agent" => Some("agents"),
        "tool" => Some("tools"),
        "frontend" => Some("frontends"),
        _ => None,
    }
}

fn billing_nodes(ports: &FinancePorts) -> Map<String, Value> {
    ports
        .ledger
        .get_doc(FinanceDoc::BillingNodes, "", "nodes")
        .unwrap_or_default()
}

fn put_billing_nodes(ports: &FinancePorts, nodes: &Value) -> Result<(), ApplicationError> {
    ports
        .ledger
        .put_doc(FinanceDoc::BillingNodes, "", "nodes", nodes, true)
        .map_err(ApplicationError::from)
}

fn market_doc(ports: &FinancePorts, bucket: &str) -> Map<String, Value> {
    ports
        .ledger
        .get_doc(FinanceDoc::Market, bucket, bucket)
        .unwrap_or_default()
}

fn put_market_doc(
    ports: &FinancePorts,
    bucket: &str,
    entries: &Value,
) -> Result<(), ApplicationError> {
    ports
        .ledger
        .put_doc(FinanceDoc::Market, bucket, bucket, entries, true)
        .map_err(ApplicationError::from)
}

fn billing_current(ports: &FinancePorts) -> Map<String, Value> {
    ports
        .ledger
        .get_doc(FinanceDoc::BillingCurrent, "", "current")
        .unwrap_or_default()
}

fn billing_catalog(
    ports: &FinancePorts,
    version: &str,
) -> Result<Map<String, Value>, ApplicationError> {
    ports
        .ledger
        .get_doc(FinanceDoc::BillingCatalog, version, "catalog")
        .map_err(ApplicationError::from)
}

fn put_billing_catalog(
    ports: &FinancePorts,
    version: &str,
    catalog: &Value,
) -> Result<(), ApplicationError> {
    ports
        .ledger
        .put_doc(
            FinanceDoc::BillingCatalog,
            version,
            "catalog",
            catalog,
            false,
        )
        .map_err(ApplicationError::from)
}

fn billing_quote(
    ports: &FinancePorts,
    quote_id: &str,
) -> Result<Map<String, Value>, ApplicationError> {
    ports
        .ledger
        .get_doc(FinanceDoc::BillingQuote, quote_id, "quote")
        .map_err(|error| match error {
            PortError::NotFound => denied("billing quote not found"),
            other => ApplicationError::Port(other),
        })
}

fn put_billing_quote(
    ports: &FinancePorts,
    quote_id: &str,
    quote: &Value,
) -> Result<(), ApplicationError> {
    ports
        .ledger
        .put_doc(FinanceDoc::BillingQuote, quote_id, "quote", quote, false)
        .map_err(ApplicationError::from)
}

// ── Wire input types ──────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PublishFinanceCatalogInput {
    #[serde(default)]
    pub catalog: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RegisterFinanceNodeInput {
    #[serde(default)]
    pub node: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RetireFinanceNodeInput {
    #[serde(rename = "nodeOwnerAccountId", default)]
    pub node_owner_account_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RegisterFinanceResourceInput {
    #[serde(default)]
    pub resource: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReviewFinanceResourceInput {
    #[serde(rename = "resourceId", default)]
    pub resource_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RetireFinanceResourceInput {
    #[serde(rename = "resourceId", default)]
    pub resource_id: String,
    #[serde(default)]
    pub kind: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PublishFinanceQuoteInput {
    #[serde(default)]
    pub quote: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CreateHoldInput {
    #[serde(rename = "quoteId", default)]
    pub quote_id: String,
    #[serde(rename = "pricingVersion", default)]
    pub pricing_version: String,
    #[serde(rename = "maxAmount", default)]
    pub max_amount: i64,
    #[serde(rename = "settlementAuthority", default)]
    pub settlement_authority: String,
    #[serde(rename = "meterProgramId", default)]
    pub meter_program_id: String,
    #[serde(rename = "expiresAt", default)]
    pub expires_at: i64,
    #[serde(rename = "idempotencyKey", default)]
    pub idempotency_key: String,
    #[serde(rename = "contextHash", default)]
    pub context_hash: String,
    #[serde(rename = "beneficiaryPlanHash", default)]
    pub beneficiary_plan_hash: String,
    #[serde(default)]
    pub beneficiaries: Vec<HoldBeneficiaryInput>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HoldBeneficiaryInput {
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub role: String,
    #[serde(rename = "maxAmount", default)]
    pub max_amount: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StartHoldInput {
    #[serde(rename = "holdId", default)]
    pub hold_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "quoteId", default)]
    pub quote_id: String,
    #[serde(rename = "runId", default)]
    pub run_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SettleHoldInput {
    #[serde(rename = "holdId", default)]
    pub hold_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "quoteId", default)]
    pub quote_id: String,
    #[serde(rename = "settlementId", default)]
    pub settlement_id: String,
    #[serde(rename = "usageHash", default)]
    pub usage_hash: String,
    #[serde(default)]
    pub lines: Vec<SettlementLineInput>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SettlementLineInput {
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub amount: i64,
    #[serde(rename = "sourceRef", default)]
    pub source_ref: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReleaseHoldInput {
    #[serde(rename = "holdId", default)]
    pub hold_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "releaseId", default)]
    pub release_id: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GetHoldInput {
    #[serde(rename = "holdId", default)]
    pub hold_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GetFinancialAccountInput {
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub limit: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RequestPayoutInput {
    #[serde(rename = "requestId", default)]
    pub request_id: String,
    #[serde(default)]
    pub amount: i64,
    #[serde(rename = "destinationRef", default)]
    pub destination_ref: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ResolvePayoutInput {
    #[serde(rename = "payoutId", default)]
    pub payout_id: String,
    #[serde(rename = "resolutionId", default)]
    pub resolution_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(rename = "providerReference", default)]
    pub provider_reference: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ListPayoutsInput {
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub limit: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpenPoolInput {
    #[serde(rename = "maxAmount", default)]
    pub max_amount: i64,
    #[serde(rename = "settlementAuthority", default)]
    pub settlement_authority: String,
    #[serde(rename = "meterProgramId", default)]
    pub meter_program_id: String,
    #[serde(rename = "expiresAt", default)]
    pub expires_at: i64,
    #[serde(rename = "idempotencyKey", default)]
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RefreshPoolInput {
    #[serde(rename = "poolId", default)]
    pub pool_id: String,
    #[serde(rename = "refreshId", default)]
    pub refresh_id: String,
    #[serde(default)]
    pub amount: i64,
    #[serde(rename = "expiresAt", default)]
    pub expires_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClosePoolInput {
    #[serde(rename = "poolId", default)]
    pub pool_id: String,
    #[serde(rename = "closeId", default)]
    pub close_id: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReservePoolInput {
    #[serde(rename = "poolId", default)]
    pub pool_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "quoteId", default)]
    pub quote_id: String,
    #[serde(rename = "runId", default)]
    pub run_id: String,
    #[serde(rename = "maxAmount", default)]
    pub max_amount: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SettlePoolInput {
    #[serde(rename = "poolId", default)]
    pub pool_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "quoteId", default)]
    pub quote_id: String,
    #[serde(rename = "runId", default)]
    pub run_id: String,
    #[serde(rename = "settlementId", default)]
    pub settlement_id: String,
    #[serde(rename = "usageHash", default)]
    pub usage_hash: String,
    #[serde(default)]
    pub lines: Vec<SettlementLineInput>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReleasePoolInput {
    #[serde(rename = "poolId", default)]
    pub pool_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "runId", default)]
    pub run_id: String,
    #[serde(rename = "releaseId", default)]
    pub release_id: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DebitPoolInput {
    #[serde(rename = "poolId", default)]
    pub pool_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "quoteId", default)]
    pub quote_id: String,
    #[serde(rename = "runId", default)]
    pub run_id: String,
    #[serde(rename = "debitId", default)]
    pub debit_id: String,
    #[serde(rename = "usageHash", default)]
    pub usage_hash: String,
    #[serde(default)]
    pub lines: Vec<SettlementLineInput>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReconcileFinancialSystemInput {
    #[serde(rename = "maxIssues", default)]
    pub max_issues: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PaymentAdjustmentInput {
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub amount: i64,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub reference: String,
    #[serde(rename = "idempotencyKey", default)]
    pub idempotency_key: String,
    #[serde(default)]
    pub metadata: Value,
}

mod actions;

pub use actions::*;
