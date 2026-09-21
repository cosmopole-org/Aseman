//! Legacy finance action family isolated from creature lifecycle ownership.
//!
//! Behavior remains characterized and unchanged here until Phase 8 replaces it with
//! typed finance ports/use cases and passes the replacement and deletion gates.

use super::*;

const FINANCE_HOLD_MAX_TTL_MS: i64 = 24 * 60 * 60 * 1000;
// A federated run may pay one agent creator/provider/platform, its execution
// node, and distinct owner/node pairs for every attached tool. Keep this bound
// finite for transaction size while allowing the quoted eight-tool maximum to
// span independent nodes.
const FINANCE_MAX_BENEFICIARIES: usize = 64;

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

#[cfg(test)]
mod finance_origin_tests {
    use super::valid_finance_origin;

    #[test]
    fn accepts_caspar_endpoint_origins_and_legacy_ids() {
        for origin in [
            "global",
            "http://localhost:8074",
            "https://node.example:8076",
            "ws://127.0.0.1:8074/",
            "wss://node.example",
        ] {
            assert!(
                valid_finance_origin(origin),
                "expected valid origin: {origin}"
            );
        }
    }

    #[test]
    fn rejects_unsafe_or_non_base_origins() {
        for origin in [
            "",
            "ftp://node.example",
            "http://",
            "http://user@node.example",
            "http://node.example/path",
            "http://node.example?query=1",
            "http://node.example#fragment",
        ] {
            assert!(
                !valid_finance_origin(origin),
                "expected invalid origin: {origin}"
            );
        }
    }
}

fn finance_hash(value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(hex::encode(hasher.finalize()))
}
fn finance_beneficiary_plan_hash(
    beneficiaries: &[crate::shell::api::packets::creatures::HoldBeneficiaryInput],
) -> String {
    let mut hasher = Sha256::new();
    for beneficiary in beneficiaries {
        hasher.update(beneficiary.user_id.as_bytes());
        hasher.update([0]);
        hasher.update(beneficiary.role.as_bytes());
        hasher.update([0]);
        hasher.update(beneficiary.max_amount.to_string().as_bytes());
        hasher.update([b'\n']);
    }
    hex::encode(hasher.finalize())
}

fn finance_hold_key(hold_id: &str) -> String {
    format!("Json::FinanceHold::{hold_id}")
}

fn get_finance_hold(trx: &dyn ITrx, hold_id: &str) -> Result<Map<String, Value>> {
    trx.get_json(&finance_hold_key(hold_id), "hold")
        .map_err(|_| anyhow!("hold not found"))
}

fn put_finance_hold(trx: &dyn ITrx, hold_id: &str, hold: &Map<String, Value>) -> Result<()> {
    trx.put_json(
        &finance_hold_key(hold_id),
        "hold",
        &Value::Object(hold.clone()),
        false,
    )
}

fn finance_pool_key(pool_id: &str) -> String {
    format!("Json::FinancePool::{pool_id}")
}

fn get_finance_pool(trx: &dyn ITrx, pool_id: &str) -> Result<Map<String, Value>> {
    trx.get_json(&finance_pool_key(pool_id), "pool")
        .map_err(|_| anyhow!("pool not found"))
}

fn put_finance_pool(trx: &dyn ITrx, pool_id: &str, pool: &Map<String, Value>) -> Result<()> {
    trx.put_json(
        &finance_pool_key(pool_id),
        "pool",
        &Value::Object(pool.clone()),
        false,
    )
}

fn finance_pool_reservation_key(run_id: &str) -> String {
    format!("Json::FinancePoolReservation::{run_id}")
}

// Per-run accumulator for live pool debits (debitPool). One record per run holds
// the running `chargedTotal` and per-beneficiary `credits` so reconciliation can
// replay a live-debited run's spend/earnings exactly as it replays a settled
// reservation. Named `FinanceLiveDebit::` (not `FinancePool*`) so it never
// collides with the pool/reservation prefix scans.
fn finance_live_debit_key(run_id: &str) -> String {
    format!("Json::FinanceLiveDebit::{run_id}")
}

fn finance_held_amount(trx: &dyn ITrx, payer_id: &str) -> Result<i64> {
    let raw = trx.get_link(&format!("FinanceHeld::{payer_id}"));
    if raw.is_empty() {
        return Ok(0);
    }
    let amount = raw
        .parse::<i64>()
        .map_err(|_| anyhow!("invalid held balance"))?;
    if amount < 0 {
        return Err(anyhow!("invalid held balance"));
    }
    Ok(amount)
}

fn set_finance_held_amount(trx: &dyn ITrx, payer_id: &str, amount: i64) -> Result<()> {
    if amount < 0 {
        return Err(anyhow!("held balance underflow"));
    }
    trx.put_link(&format!("FinanceHeld::{payer_id}"), &amount.to_string());
    Ok(())
}

pub(super) fn finance_debt_amount(trx: &dyn ITrx, user_id: &str) -> Result<i64> {
    let raw = trx.get_link(&format!("FinanceDebt::{user_id}"));
    if raw.is_empty() {
        return Ok(0);
    }
    let amount = raw
        .parse::<i64>()
        .map_err(|_| anyhow!("invalid wallet debt"))?;
    if amount < 0 {
        return Err(anyhow!("invalid wallet debt"));
    }
    Ok(amount)
}

pub(super) fn set_finance_debt_amount(trx: &dyn ITrx, user_id: &str, amount: i64) -> Result<()> {
    if amount < 0 {
        return Err(anyhow!("wallet debt underflow"));
    }
    trx.put_link(&format!("FinanceDebt::{user_id}"), &amount.to_string());
    Ok(())
}

pub(super) fn finance_withdrawable_amount(trx: &dyn ITrx, user_id: &str) -> Result<i64> {
    finance_counter(trx, &format!("FinanceWithdrawable::{user_id}"))
}

pub(super) fn set_finance_withdrawable_amount(
    trx: &dyn ITrx,
    user_id: &str,
    amount: i64,
) -> Result<()> {
    if amount < 0 {
        return Err(anyhow!("withdrawable balance underflow"));
    }
    trx.put_link(
        &format!("FinanceWithdrawable::{user_id}"),
        &amount.to_string(),
    );
    Ok(())
}

fn finance_payout_held_amount(trx: &dyn ITrx, user_id: &str) -> Result<i64> {
    finance_counter(trx, &format!("FinancePayoutHeld::{user_id}"))
}

fn set_finance_payout_held_amount(trx: &dyn ITrx, user_id: &str, amount: i64) -> Result<()> {
    if amount < 0 {
        return Err(anyhow!("payout held balance underflow"));
    }
    trx.put_link(
        &format!("FinancePayoutHeld::{user_id}"),
        &amount.to_string(),
    );
    Ok(())
}

fn finance_counter(trx: &dyn ITrx, key: &str) -> Result<i64> {
    let raw = trx.get_link(key);
    if raw.is_empty() {
        return Ok(0);
    }
    let value = raw
        .parse::<i64>()
        .map_err(|_| anyhow!("invalid finance counter"))?;
    if value < 0 {
        return Err(anyhow!("invalid finance counter"));
    }
    Ok(value)
}

fn add_finance_counter(trx: &dyn ITrx, key: &str, amount: i64) -> Result<i64> {
    if amount < 0 {
        return Err(anyhow!("finance counter amount must be nonnegative"));
    }
    let next = finance_counter(trx, key)?
        .checked_add(amount)
        .ok_or_else(|| anyhow!("finance counter overflow"))?;
    trx.put_link(key, &next.to_string());
    Ok(next)
}

fn finance_project_budget_key(project_id: &str) -> String {
    format!("Json::FinanceProjectBudget::{project_id}")
}

fn finance_nonnegative_field(map: &Map<String, Value>, field: &str) -> Result<i64> {
    let Some(value) = map.get(field) else {
        return Ok(0);
    };
    let amount = value
        .as_i64()
        .ok_or_else(|| anyhow!("invalid project budget field: {field}"))?;
    if amount < 0 {
        return Err(anyhow!("invalid project budget field: {field}"));
    }
    Ok(amount)
}

fn reserve_project_budget(trx: &dyn ITrx, project_id: &str, amount: i64, now: i64) -> Result<()> {
    if project_id.is_empty() {
        return Ok(());
    }
    if !trx.has_obj(Store::type_(), project_id) {
        return Err(anyhow!("project not found"));
    }
    let metadata = trx
        .get_json(&format!("StoreMeta::{project_id}"), "metadata")
        .unwrap_or_default();
    let configured_budget = finance_nonnegative_field(&metadata, "budgetMinor")?;
    let metadata_spent = finance_nonnegative_field(&metadata, "spentMinor")?;
    let mut state = trx
        .get_json(&finance_project_budget_key(project_id), "budget")
        .unwrap_or_default();
    let ledger_spent = finance_nonnegative_field(&state, "spentMinor")?;
    let spent = ledger_spent.max(metadata_spent);
    let reserved = finance_nonnegative_field(&state, "reservedMinor")?;
    let committed = spent
        .checked_add(reserved)
        .and_then(|value| value.checked_add(amount))
        .ok_or_else(|| anyhow!("project budget overflow"))?;
    if configured_budget > 0 && committed > configured_budget {
        return Err(anyhow!("project budget exceeded"));
    }
    state.insert("projectId".to_string(), json!(project_id));
    state.insert("budgetMinor".to_string(), json!(configured_budget));
    state.insert("spentMinor".to_string(), json!(spent));
    state.insert(
        "reservedMinor".to_string(),
        json!(reserved
            .checked_add(amount)
            .ok_or_else(|| anyhow!("project reservation overflow"))?),
    );
    state.insert("updatedAt".to_string(), json!(now));
    trx.put_json(
        &finance_project_budget_key(project_id),
        "budget",
        &Value::Object(state),
        false,
    )
}

fn finalize_project_budget(
    trx: &dyn ITrx,
    project_id: &str,
    reserved_amount: i64,
    spent_amount: i64,
    now: i64,
) -> Result<()> {
    if project_id.is_empty() {
        return Ok(());
    }
    let mut state = trx
        .get_json(&finance_project_budget_key(project_id), "budget")
        .map_err(|_| anyhow!("project budget reservation not found"))?;
    let reserved = finance_nonnegative_field(&state, "reservedMinor")?;
    let spent = finance_nonnegative_field(&state, "spentMinor")?;
    let remaining = reserved
        .checked_sub(reserved_amount)
        .ok_or_else(|| anyhow!("project budget reservation underflow"))?;
    let total_spent = spent
        .checked_add(spent_amount)
        .ok_or_else(|| anyhow!("project spend overflow"))?;
    state.insert("reservedMinor".to_string(), json!(remaining));
    state.insert("spentMinor".to_string(), json!(total_spent));
    state.insert("updatedAt".to_string(), json!(now));
    trx.put_json(
        &finance_project_budget_key(project_id),
        "budget",
        &Value::Object(state),
        false,
    )
}

pub(super) fn write_finance_journal(
    trx: &dyn ITrx,
    kind: &str,
    hold_id: &str,
    payer_id: &str,
    payload: Value,
    participants: &[String],
    now: i64,
) -> Result<String> {
    let journal_id = secure_unique_string();
    let entry = json!({
        "journalId": journal_id,
        "kind": kind,
        "holdId": hold_id,
        "payerUserId": payer_id,
        "createdAt": now,
        "payload": payload,
    });
    trx.put_json(
        &format!("Json::FinanceJournal::{journal_id}"),
        "entry",
        &entry,
        false,
    )?;

    let mut indexed: HashMap<&str, bool> = HashMap::new();
    for participant in participants {
        if participant.is_empty() || indexed.insert(participant.as_str(), true).is_some() {
            continue;
        }
        trx.put_link(
            &format!("FinanceJournalByUser::{participant}::{now:020}::{journal_id}"),
            &journal_id,
        );
    }
    Ok(journal_id)
}

const FEDERATED_FINANCE_MAX_RECORD_BYTES: usize = 256 * 1024;

fn federated_finance_object(value: Value, label: &str) -> Result<Map<String, Value>> {
    let object = value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("{label} must be an object"))?;
    if serde_json::to_vec(&object)?.len() > FEDERATED_FINANCE_MAX_RECORD_BYTES {
        return Err(anyhow!("{label} is too large"));
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

fn publish_finance_catalog(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<PublishFinanceCatalogInput, _>(
        app,
        "/creatures/publishFinanceCatalog",
        finance_guard(),
        move |state: Arc<dyn IState>, input: PublishFinanceCatalogInput| -> Result<Value> {
            let trx = state.trx();
            let caller = state.info().user_id();
            if caller != "1@global" {
                return Err(anyhow!("global platform owner required"));
            }
            let catalog = federated_finance_object(input.catalog, "finance catalog")?;
            let version = catalog
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if !valid_finance_id(&version)
                || !federated_finance_safe_numbers(&Value::Object(catalog.clone()))
            {
                return Err(anyhow!("invalid finance catalog"));
            }
            for key in [
                "tokenScale",
                "defaultInputPerMillionMinor",
                "defaultOutputPerMillionMinor",
                "sandboxPerMinuteMinor",
                "minChargeMinor",
                "platformCommissionBps",
                "authorizationSafetyBps",
                "quoteTtlMs",
                "holdTtlMs",
            ] {
                if catalog.get(key).and_then(Value::as_i64).is_none() {
                    return Err(anyhow!("invalid finance catalog integer: {key}"));
                }
            }
            if catalog
                .get("tokenScale")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                <= 0
                || catalog
                    .get("sandboxPerMinuteMinor")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    <= 0
            {
                return Err(anyhow!("tokenScale and sandbox rate must be positive"));
            }
            let commission = catalog
                .get("platformCommissionBps")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let safety = catalog
                .get("authorizationSafetyBps")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let quote_ttl = catalog
                .get("quoteTtlMs")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let hold_ttl = catalog
                .get("holdTtlMs")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if commission > 10_000
                || !(10_000..=100_000).contains(&safety)
                || quote_ttl > hold_ttl
                || hold_ttl > FINANCE_HOLD_MAX_TTL_MS
            {
                return Err(anyhow!("invalid finance catalog policy"));
            }
            for key in [
                "settlementAuthority",
                "platformAccountId",
                "providerClearingAccountId",
                "nodeOwnerAccountId",
            ] {
                let account = catalog.get(key).and_then(Value::as_str).unwrap_or("");
                if !valid_finance_id(account)
                    || (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                        .account(account)?
                        .is_none()
                {
                    return Err(anyhow!("invalid finance catalog account: {key}"));
                }
            }
            let catalog_value = Value::Object(catalog.clone());
            let catalog_hash = finance_hash(&catalog_value)?;
            let key = format!("Json::BillingCatalog::{version}");
            let mut already_published = false;
            if let Ok(existing) = trx.get_json(&key, "catalog") {
                if !existing.is_empty() {
                    if Value::Object(existing.clone()) != catalog_value {
                        return Err(anyhow!("pricing version is immutable"));
                    }
                    already_published = true;
                }
            }
            trx.put_json(&key, "catalog", &catalog_value, false)?;
            trx.put_json(
                "Json::CreatureNamespace::billing",
                "current",
                &json!({"version": version, "catalogHash": catalog_hash}),
                false,
            )?;
            Ok(json!({
                "ok": true,
                "catalog": catalog,
                "catalogHash": catalog_hash,
                "alreadyPublished": already_published,
            }))
        },
    )
}

fn register_finance_node(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<RegisterFinanceNodeInput, _>(
        app,
        "/creatures/registerFinanceNode",
        finance_guard(),
        move |state: Arc<dyn IState>, input: RegisterFinanceNodeInput| -> Result<Value> {
            let trx = state.trx();
            let caller = state.info().user_id();
            let mut node = federated_finance_object(input.node, "finance node")?;
            let owner = node
                .get("nodeOwnerAccountId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let authority = node
                .get("settlementAuthority")
                .and_then(Value::as_str)
                .unwrap_or("");
            let origin = node.get("originId").and_then(Value::as_str).unwrap_or("");
            let meter = node
                .get("meterProgramId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let talent_meter = node
                .get("talentMeterProgramId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let meter_creature = node
                .get("meterCreatureId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let meter_entity = node
                .get("meterEntityId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let talent_meter_creature = node
                .get("talentMeterCreatureId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let talent_meter_entity = node
                .get("talentMeterEntityId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let revision = node.get("revision").and_then(Value::as_str).unwrap_or("");
            let rate = node
                .get("sandboxPerMinuteMinor")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if caller != owner
                || authority != caller
                || !valid_finance_id(&caller)
                || !valid_finance_origin(origin)
                || !valid_finance_id(meter)
                || !valid_finance_id(talent_meter)
                || !valid_finance_id(meter_creature)
                || !valid_finance_id(meter_entity)
                || !valid_finance_id(talent_meter_creature)
                || !valid_finance_id(talent_meter_entity)
                || !valid_finance_hash(revision)
                || rate <= 0
                || rate > 9_007_199_254_740_991
                || (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .account(&caller)?
                    .is_none()
            {
                return Err(anyhow!("invalid host-attested finance node registration"));
            }
            let now = Utc::now().timestamp_millis();
            node.insert("status".into(), json!("active"));
            node.insert("updatedAt".into(), json!(now));
            trx.put_json(
                "Json::CreatureNamespace::billing",
                "nodes",
                &json!({caller.clone(): Value::Object(node.clone())}),
                true,
            )?;
            Ok(json!({"ok": true, "node": node}))
        },
    )
}

fn retire_finance_node(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<RetireFinanceNodeInput, _>(
        app,
        "/creatures/retireFinanceNode",
        finance_guard(),
        move |state: Arc<dyn IState>, input: RetireFinanceNodeInput| -> Result<Value> {
            let trx = state.trx();
            let caller = state.info().user_id();
            if input.node_owner_account_id != caller || !valid_finance_id(&caller) {
                return Err(anyhow!("node owner mismatch"));
            }
            let nodes = trx
                .get_json("Json::CreatureNamespace::billing", "nodes")
                .unwrap_or_default();
            let mut node = nodes
                .get(&caller)
                .and_then(Value::as_object)
                .cloned()
                .ok_or_else(|| anyhow!("finance node not found"))?;
            let now = Utc::now().timestamp_millis();
            node.insert("status".into(), json!("retired"));
            node.insert("updatedAt".into(), json!(now));
            node.insert(
                "revision".into(),
                json!(finance_hash(&json!({
                    "prior": node.get("revision"), "status": "retired", "updatedAt": now
                }))?),
            );
            trx.put_json(
                "Json::CreatureNamespace::billing",
                "nodes",
                &json!({caller.clone(): Value::Object(node.clone())}),
                true,
            )?;
            Ok(json!({"ok": true, "node": node}))
        },
    )
}

fn register_finance_resource(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<RegisterFinanceResourceInput, _>(
        app,
        "/creatures/registerFinanceResource",
        finance_guard(),
        move |state: Arc<dyn IState>, input: RegisterFinanceResourceInput| -> Result<Value> {
            let trx = state.trx();
            let caller = state.info().user_id();
            let mut resource = federated_finance_object(input.resource, "finance resource")?;
            let resource_id = resource
                .get("programId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let kind = resource
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let owner = resource
                .get("owner")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let host_owner = resource
                .get("hostNodeOwnerAccountId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let bucket = federated_finance_market_bucket(&kind)
                .ok_or_else(|| anyhow!("invalid finance resource kind"))?;
            let pricing = resource.get("pricing").cloned().unwrap_or(Value::Null);
            // Program records are execution-node state. The finance host bridge
            // resolves Program -> Machine -> owner locally and overwrites these
            // fields before the node owner signs this global attestation.
            if caller != host_owner
                || !valid_finance_id(&resource_id)
                || !valid_finance_id(&owner)
                || (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .account(&owner)?
                    .is_none()
                || !federated_finance_safe_numbers(&pricing)
            {
                return Err(anyhow!("invalid host-attested finance resource"));
            }
            let nodes = trx
                .get_json("Json::CreatureNamespace::billing", "nodes")
                .unwrap_or_default();
            let node = nodes
                .get(&host_owner)
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow!("finance execution node not registered"))?;
            if node.get("status").and_then(Value::as_str) != Some("active")
                || resource.get("hostOriginId").and_then(Value::as_str)
                    != node.get("originId").and_then(Value::as_str)
                || resource
                    .get("billingMeterProgramId")
                    .and_then(Value::as_str)
                    != node.get("meterProgramId").and_then(Value::as_str)
                || resource
                    .get("billingMeterCreatureId")
                    .and_then(Value::as_str)
                    != node.get("meterCreatureId").and_then(Value::as_str)
                || resource.get("billingMeterEntityId").and_then(Value::as_str)
                    != node.get("meterEntityId").and_then(Value::as_str)
                || resource
                    .get("nodeRegistrationRevision")
                    .and_then(Value::as_str)
                    != node.get("revision").and_then(Value::as_str)
                || resource
                    .get("nodeSandboxPerMinuteMinor")
                    .and_then(Value::as_i64)
                    != node.get("sandboxPerMinuteMinor").and_then(Value::as_i64)
            {
                return Err(anyhow!("resource does not match its active finance node"));
            }
            let entries = trx
                .get_json("Json::CreatureNamespace::market", bucket)
                .unwrap_or_default();
            let existing = entries.get(&resource_id).and_then(Value::as_object);
            if let Some(existing) = existing {
                if existing
                    .get("hostNodeOwnerAccountId")
                    .and_then(Value::as_str)
                    != Some(host_owner.as_str())
                {
                    return Err(anyhow!("resource migration requires a new program id"));
                }
            }
            let requested_status = resource
                .get("status")
                .and_then(Value::as_str)
                .filter(|status| caller == "1@global" && matches!(*status, "approved" | "denied"))
                .unwrap_or("pending");
            let preserved_status = existing
                .and_then(|row| row.get("status"))
                .and_then(Value::as_str)
                .filter(|status| matches!(*status, "approved" | "denied"))
                .unwrap_or(requested_status);
            resource.insert("status".into(), json!(preserved_status));
            resource.insert("federated".into(), json!(true));
            resource.insert("registeredAt".into(), json!(Utc::now().timestamp_millis()));
            trx.put_json(
                "Json::CreatureNamespace::market",
                bucket,
                &json!({resource_id.clone(): Value::Object(resource.clone())}),
                true,
            )?;
            Ok(json!({"ok": true, "resource": resource}))
        },
    )
}

fn review_finance_resource(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ReviewFinanceResourceInput, _>(
        app,
        "/creatures/reviewFinanceResource",
        finance_guard(),
        move |state: Arc<dyn IState>, input: ReviewFinanceResourceInput| -> Result<Value> {
            let trx = state.trx();
            let caller = state.info().user_id();
            if caller != "1@global" || !matches!(input.status.as_str(), "approved" | "denied") {
                return Err(anyhow!("global finance reviewer required"));
            }
            let bucket = federated_finance_market_bucket(&input.kind)
                .ok_or_else(|| anyhow!("invalid finance resource kind"))?;
            let entries = trx
                .get_json("Json::CreatureNamespace::market", bucket)
                .unwrap_or_default();
            let mut resource = entries
                .get(&input.resource_id)
                .and_then(Value::as_object)
                .cloned()
                .ok_or_else(|| anyhow!("finance resource not found"))?;
            resource.insert("status".into(), json!(input.status));
            resource.insert("reviewedBy".into(), json!(caller));
            resource.insert("reviewedAt".into(), json!(Utc::now().timestamp_millis()));
            if !input.reason.is_empty() {
                resource.insert("reason".into(), json!(input.reason));
            }
            trx.put_json(
                "Json::CreatureNamespace::market",
                bucket,
                &json!({input.resource_id.clone(): Value::Object(resource.clone())}),
                true,
            )?;
            Ok(json!({"ok": true, "resource": resource}))
        },
    )
}

fn retire_finance_resource(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<RetireFinanceResourceInput, _>(
        app,
        "/creatures/retireFinanceResource",
        finance_guard(),
        move |state: Arc<dyn IState>, input: RetireFinanceResourceInput| -> Result<Value> {
            let trx = state.trx();
            let caller = state.info().user_id();
            let bucket = federated_finance_market_bucket(&input.kind)
                .ok_or_else(|| anyhow!("invalid finance resource kind"))?;
            let entries = trx
                .get_json("Json::CreatureNamespace::market", bucket)
                .unwrap_or_default();
            let resource = entries
                .get(&input.resource_id)
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow!("finance resource not found"))?;
            let host_owner = resource
                .get("hostNodeOwnerAccountId")
                .and_then(Value::as_str)
                .unwrap_or("");
            if caller != host_owner && caller != "1@global" {
                return Err(anyhow!("resource host or global reviewer required"));
            }
            trx.put_json(
                "Json::CreatureNamespace::market",
                bucket,
                &json!({input.resource_id.clone(): Value::Null}),
                true,
            )?;
            Ok(json!({"ok": true, "resourceId": input.resource_id}))
        },
    )
}

fn validate_federated_quote_resource(trx: &dyn ITrx, execution: &Map<String, Value>) -> Result<()> {
    let resource_id = execution
        .get("resourceId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let kind = execution.get("kind").and_then(Value::as_str).unwrap_or("");
    let bucket = federated_finance_market_bucket(kind)
        .ok_or_else(|| anyhow!("invalid quote execution resource kind"))?;
    if !valid_finance_id(resource_id) {
        return Err(anyhow!("invalid quote execution resource id"));
    }
    let entries = trx
        .get_json("Json::CreatureNamespace::market", bucket)
        .unwrap_or_default();
    let resource = entries
        .get(resource_id)
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("quoted resource is not globally registered"))?;
    if resource.get("status").and_then(Value::as_str) != Some("approved") {
        return Err(anyhow!("quoted resource is not globally approved"));
    }
    let node_owner = resource
        .get("hostNodeOwnerAccountId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let nodes = trx
        .get_json("Json::CreatureNamespace::billing", "nodes")
        .unwrap_or_default();
    let node = nodes
        .get(node_owner)
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("quoted resource node is not registered"))?;
    if node.get("status").and_then(Value::as_str) != Some("active")
        || execution.get("nodeOwnerAccountId") != resource.get("hostNodeOwnerAccountId")
        || execution.get("hostOriginId") != resource.get("hostOriginId")
        || execution.get("meterProgramId") != resource.get("billingMeterProgramId")
        || execution.get("meterCreatureId") != resource.get("billingMeterCreatureId")
        || execution.get("meterEntityId") != resource.get("billingMeterEntityId")
        || execution.get("nodeRegistrationRevision") != resource.get("nodeRegistrationRevision")
        || execution.get("sandboxPerMinuteMinor") != resource.get("nodeSandboxPerMinuteMinor")
        || resource.get("hostOriginId") != node.get("originId")
        || resource.get("billingMeterProgramId") != node.get("meterProgramId")
        || resource.get("billingMeterCreatureId") != node.get("meterCreatureId")
        || resource.get("billingMeterEntityId") != node.get("meterEntityId")
        || resource.get("nodeRegistrationRevision") != node.get("revision")
        || resource.get("nodeSandboxPerMinuteMinor") != node.get("sandboxPerMinuteMinor")
        || execution.get("settlementAuthority") != node.get("settlementAuthority")
    {
        return Err(anyhow!(
            "quote execution does not match the active global resource binding"
        ));
    }
    Ok(())
}

fn publish_finance_quote(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<PublishFinanceQuoteInput, _>(
        app,
        "/creatures/publishFinanceQuote",
        finance_guard(),
        move |state: Arc<dyn IState>, input: PublishFinanceQuoteInput| -> Result<Value> {
            let trx = state.trx();
            let caller = state.info().user_id();
            let mut quote = federated_finance_object(input.quote, "finance quote")?;
            let quote_id = quote
                .get("quoteId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let payer = quote
                .get("payerUserId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let max_amount = quote.get("maxAmount").and_then(Value::as_i64).unwrap_or(0);
            let hold = quote
                .get("holdRequest")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow!("quote holdRequest missing"))?;
            let execution = quote
                .get("executionPlan")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow!("quote executionPlan missing"))?;
            let quote_kind = quote.get("kind").and_then(Value::as_str).unwrap_or("");
            let authority = execution
                .get("settlementAuthority")
                .and_then(Value::as_str)
                .unwrap_or("");
            let meter = execution
                .get("meterProgramId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let meter_creature = execution
                .get("meterCreatureId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let meter_entity = execution
                .get("meterEntityId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let pricing_version = quote
                .get("pricingVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let active_catalog = trx
                .get_json("Json::CreatureNamespace::billing", "current")
                .unwrap_or_default();
            let catalog_exists = !pricing_version.is_empty()
                && trx
                    .get_json(
                        &format!("Json::BillingCatalog::{pricing_version}"),
                        "catalog",
                    )
                    .map(|catalog| !catalog.is_empty())
                    .unwrap_or(false);
            let nodes = trx
                .get_json("Json::CreatureNamespace::billing", "nodes")
                .unwrap_or_default();
            let issuer_node = nodes.get(&caller).and_then(Value::as_object);
            let coordinator_node = nodes.get(authority).and_then(Value::as_object);
            let expected_meter = coordinator_node.and_then(|node| {
                if quote_kind == "talent" {
                    node.get("talentMeterProgramId")
                } else {
                    node.get("meterProgramId")
                }
            });
            if !valid_finance_id(&quote_id)
                || !valid_finance_id(&payer)
                || !matches!(quote_kind, "agent" | "tool" | "talent")
                || active_catalog.get("version").and_then(Value::as_str) != Some(pricing_version)
                || !catalog_exists
                || max_amount <= 0
                || (quote_kind == "talent" && authority != caller)
                || issuer_node
                    .and_then(|node| node.get("status"))
                    .and_then(Value::as_str)
                    != Some("active")
                || coordinator_node
                    .and_then(|node| node.get("status"))
                    .and_then(Value::as_str)
                    != Some("active")
                || expected_meter.and_then(Value::as_str) != Some(meter)
                || (quote_kind != "talent"
                    && (coordinator_node
                        .and_then(|node| node.get("meterCreatureId"))
                        .and_then(Value::as_str)
                        != Some(meter_creature)
                        || coordinator_node
                            .and_then(|node| node.get("meterEntityId"))
                            .and_then(Value::as_str)
                            != Some(meter_entity)))
                || hold.get("quoteId").and_then(Value::as_str) != Some(quote_id.as_str())
                || hold.get("maxAmount").and_then(Value::as_i64) != Some(max_amount)
                || hold.get("settlementAuthority").and_then(Value::as_str) != Some(authority)
                || hold.get("meterProgramId").and_then(Value::as_str) != Some(meter)
                || (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .account(&payer)?
                    .is_none()
                || (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .account(&caller)?
                    .is_none()
                || !federated_finance_safe_numbers(&Value::Object(quote.clone()))
            {
                return Err(anyhow!("invalid immutable finance quote"));
            }
            let resources = execution
                .get("resources")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("quote execution resources missing"))?;
            if quote_kind == "talent" {
                if !resources.is_empty() {
                    return Err(anyhow!("talent quote cannot contain execution resources"));
                }
            } else {
                if resources.is_empty() || resources.len() > 9 {
                    return Err(anyhow!("invalid quote execution resource count"));
                }
                let mut seen = HashMap::<String, bool>::new();
                for (index, raw) in resources.iter().enumerate() {
                    let row = raw
                        .as_object()
                        .ok_or_else(|| anyhow!("invalid quote execution resource"))?;
                    validate_federated_quote_resource(&*trx, row)?;
                    let resource_id = row
                        .get("resourceId")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if seen.insert(resource_id.clone(), true).is_some() {
                        return Err(anyhow!("duplicate quote execution resource"));
                    }
                    if index == 0
                        && (row.get("kind").and_then(Value::as_str) != Some(quote_kind)
                            || resource_id
                                != quote
                                    .get("resourceId")
                                    .and_then(Value::as_str)
                                    .unwrap_or(""))
                    {
                        return Err(anyhow!("quote coordinator resource mismatch"));
                    }
                }
                let coordinator = resources[0].as_object().unwrap();
                if coordinator
                    .get("settlementAuthority")
                    .and_then(Value::as_str)
                    != Some(authority)
                    || coordinator.get("meterProgramId").and_then(Value::as_str) != Some(meter)
                    || coordinator.get("meterCreatureId").and_then(Value::as_str)
                        != Some(meter_creature)
                    || coordinator.get("meterEntityId").and_then(Value::as_str)
                        != Some(meter_entity)
                {
                    return Err(anyhow!("quote coordinator execution mismatch"));
                }
            }
            let beneficiaries = hold
                .get("beneficiaries")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("quote beneficiaries missing"))?;
            if beneficiaries.is_empty() || beneficiaries.len() > FINANCE_MAX_BENEFICIARIES {
                return Err(anyhow!("invalid quote beneficiary count"));
            }
            let mut cap_total = 0_i64;
            for raw in beneficiaries {
                let row = raw
                    .as_object()
                    .ok_or_else(|| anyhow!("invalid quote beneficiary"))?;
                let user_id = row.get("userId").and_then(Value::as_str).unwrap_or("");
                let amount = row.get("maxAmount").and_then(Value::as_i64).unwrap_or(0);
                if !valid_finance_id(user_id)
                    || amount <= 0
                    || (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                        .account(user_id)?
                        .is_none()
                {
                    return Err(anyhow!("invalid quote beneficiary"));
                }
                cap_total = cap_total
                    .checked_add(amount)
                    .ok_or_else(|| anyhow!("quote beneficiary overflow"))?;
            }
            if cap_total != max_amount {
                return Err(anyhow!("quote caps do not equal maxAmount"));
            }
            let key = format!("Json::BillingQuote::{quote_id}");
            if let Ok(existing) = trx.get_json(&key, "quote") {
                if !existing.is_empty() {
                    let mut comparable = existing.clone();
                    comparable.remove("quoteIssuerNodeOwnerId");
                    comparable.remove("publishedAt");
                    if comparable != quote {
                        return Err(anyhow!("quote id is immutable"));
                    }
                    return Ok(json!({"ok": true, "alreadyPublished": true, "quote": existing}));
                }
            }
            quote.insert("quoteIssuerNodeOwnerId".into(), json!(caller));
            quote.insert("publishedAt".into(), json!(Utc::now().timestamp_millis()));
            trx.put_json(&key, "quote", &Value::Object(quote.clone()), false)?;
            Ok(json!({"ok": true, "quote": quote}))
        },
    )
}

fn create_hold(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let security_app = app.clone();
    build_secure_action::<CreateHoldInput, _>(
        app,
        "/creatures/createHold",
        finance_guard(),
        move |state: Arc<dyn IState>, input: CreateHoldInput| -> Result<Value> {
            let trx = state.trx();
            let payer_id = state.info().user_id();
            let now = Utc::now().timestamp_millis();

            if !valid_finance_id(&input.quote_id)
                || !valid_finance_id(&input.pricing_version)
                || !valid_finance_id(&input.idempotency_key)
                || !valid_finance_id(&input.settlement_authority)
                || !valid_finance_id(&input.meter_program_id)
            {
                return Err(anyhow!(
                    "invalid quote, pricing, meter, authority, or idempotency identifier"
                ));
            }
            if !valid_finance_hash(&input.context_hash)
                || !valid_finance_hash(&input.beneficiary_plan_hash)
            {
                return Err(anyhow!(
                    "contextHash and beneficiaryPlanHash must be sha256 hex"
                ));
            }
            if input.max_amount <= 0 {
                return Err(anyhow!("maxAmount must be greater than zero"));
            }
            if input.expires_at <= now
                || input.expires_at
                    > now
                        .checked_add(FINANCE_HOLD_MAX_TTL_MS)
                        .ok_or_else(|| anyhow!("hold expiry overflow"))?
            {
                return Err(anyhow!(
                    "expiresAt must be in the future and within 24 hours"
                ));
            }
            if input.beneficiaries.is_empty()
                || input.beneficiaries.len() > FINANCE_MAX_BENEFICIARIES
            {
                return Err(anyhow!(
                    "beneficiaries must contain between 1 and 64 entries"
                ));
            }

            // A hold is not an arbitrary client-authored transfer plan. Load the
            // immutable quote and require the signed request to equal the exact
            // holdRequest the pricing creature persisted.
            let quote = trx
                .get_json(&format!("Json::BillingQuote::{}", input.quote_id), "quote")
                .map_err(|_| anyhow!("billing quote not found"))?;
            if quote.get("payerUserId").and_then(Value::as_str) != Some(payer_id.as_str()) {
                return Err(anyhow!("billing quote payer mismatch"));
            }
            let quote_expires_at = quote.get("expiresAt").and_then(as_i64).unwrap_or(0);
            if quote_expires_at <= 0 || now > quote_expires_at {
                return Err(anyhow!("billing quote expired"));
            }
            let signed_request = serde_json::to_value(&input)?;
            if quote.get("holdRequest") != Some(&signed_request) {
                return Err(anyhow!("hold request does not match server quote"));
            }
            let project_id = quote
                .get("projectId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if !project_id.is_empty()
                && !security_app
                    .tools()
                    .security()
                    .has_access_to_store(&payer_id, &project_id)
            {
                return Err(anyhow!("payer is not a project member"));
            }
            if (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                .account(&input.settlement_authority)?
                .is_none()
            {
                return Err(anyhow!("settlement authority not found"));
            }
            if !trx.has_obj("Program", &input.meter_program_id) {
                return Err(anyhow!("meter program not found"));
            }

            let computed_plan_hash = finance_beneficiary_plan_hash(&input.beneficiaries);
            if computed_plan_hash != input.beneficiary_plan_hash.to_ascii_lowercase() {
                return Err(anyhow!("beneficiary plan hash mismatch"));
            }
            let request_hash = finance_hash(&serde_json::to_value(&input)?)?;
            let request_marker =
                format!("FinanceHoldRequest::{payer_id}::{}", input.idempotency_key);
            let previous = trx.get_link(&request_marker);
            if !previous.is_empty() {
                let Some((hold_id, previous_hash)) = previous.split_once('|') else {
                    return Err(anyhow!("invalid hold idempotency record"));
                };
                if previous_hash != request_hash {
                    return Err(anyhow!(
                        "idempotency key already used with different request"
                    ));
                }
                let hold = get_finance_hold(&*trx, hold_id)?;
                return Ok(json!({
                    "applied": false,
                    "alreadyApplied": true,
                    "hold": hold,
                }));
            }

            let mut cap_total = 0_i64;
            let mut caps: HashMap<String, i64> = HashMap::new();
            let mut participants = vec![payer_id.clone(), input.settlement_authority.clone()];
            for beneficiary in &input.beneficiaries {
                if !valid_finance_id(&beneficiary.user_id)
                    || !valid_finance_id(&beneficiary.role)
                    || beneficiary.max_amount <= 0
                {
                    return Err(anyhow!("invalid beneficiary"));
                }
                if beneficiary.user_id == payer_id {
                    return Err(anyhow!("payer cannot be a hold beneficiary"));
                }
                let cap_key = format!("{}|{}", beneficiary.user_id, beneficiary.role);
                if caps.insert(cap_key, beneficiary.max_amount).is_some() {
                    return Err(anyhow!("duplicate beneficiary role"));
                }
                if (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .account(&beneficiary.user_id)?
                    .is_none()
                {
                    return Err(anyhow!("beneficiary not found"));
                }
                cap_total = cap_total
                    .checked_add(beneficiary.max_amount)
                    .ok_or_else(|| anyhow!("beneficiary cap overflow"))?;
                participants.push(beneficiary.user_id.clone());
            }
            if cap_total != input.max_amount {
                return Err(anyhow!("beneficiary caps must equal maxAmount"));
            }

            let Some(mut payer) =
                (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .account(&payer_id.clone())?
            else {
                return Err(anyhow!("payer creature not found"));
            };
            if finance_debt_amount(&*trx, &payer_id)? > 0 {
                return Err(anyhow!("wallet has outstanding payment debt"));
            }
            let withdrawable = finance_withdrawable_amount(&*trx, &payer_id)?;
            if withdrawable > payer.balance {
                return Err(anyhow!("withdrawable balance exceeds available balance"));
            }
            let nonwithdrawable = payer.balance - withdrawable;
            let withdrawable_amount = input.max_amount.saturating_sub(nonwithdrawable);
            payer.balance = payer
                .balance
                .checked_sub(input.max_amount)
                .ok_or_else(|| {
                    anyhow!("insufficient available balance to authorize this run (funds may be held by another active run)")
                })?;
            // With max_amount <= balance (just checked) and withdrawable <= balance
            // (checked above), withdrawable_amount = max(0, max_amount - (balance -
            // withdrawable)) <= withdrawable, so this subtraction cannot underflow
            // on a consistent ledger. A failure here therefore means the ledger was
            // read inconsistently (a hold racing another balance mutation) — fail
            // the whole authorization cleanly (the transaction is discarded, so no
            // partial write) with an actionable message rather than leaking a raw
            // "underflow", and let reconciliation surface any real counter drift.
            set_finance_withdrawable_amount(
                &*trx,
                &payer_id,
                withdrawable.checked_sub(withdrawable_amount).ok_or_else(|| {
                    anyhow!("could not authorize this run against the current balance (concurrent authorization in progress) — please retry")
                })?,
            )?;
            let held = finance_held_amount(&*trx, &payer_id)?
                .checked_add(input.max_amount)
                .ok_or_else(|| anyhow!("held balance overflow"))?;
            reserve_project_budget(&*trx, &project_id, input.max_amount, now)?;

            let hold_id = secure_unique_string();
            let hold = json!({
                "version": 2,
                "holdId": hold_id,
                "payerUserId": payer_id,
                "quoteId": input.quote_id,
                "pricingVersion": input.pricing_version,
                "maxAmount": input.max_amount,
                "remainingAmount": input.max_amount,
                "withdrawableAmount": withdrawable_amount,
                "meterProgramId": input.meter_program_id,
                "settlementAuthority": input.settlement_authority,
                "expiresAt": input.expires_at,
                "projectId": project_id,
                "contextHash": input.context_hash,
                "beneficiaryPlanHash": input.beneficiary_plan_hash.to_ascii_lowercase(),
                "beneficiaries": input.beneficiaries,
                "requestHash": request_hash,
                "status": "open",
                "createdAt": now,
            });
            let hold_map = hold
                .as_object()
                .cloned()
                .ok_or_else(|| anyhow!("invalid hold record"))?;

            (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                .store_account(&payer)?;
            set_finance_held_amount(&*trx, &payer_id, held)?;
            put_finance_hold(&*trx, &hold_id, &hold_map)?;
            trx.put_link(&request_marker, &format!("{hold_id}|{request_hash}"));
            trx.put_link(
                &format!("FinanceHoldByPayer::{payer_id}::{now:020}::{hold_id}"),
                &hold_id,
            );
            let journal_id = write_finance_journal(
                &*trx,
                "hold.created",
                &hold_id,
                &payer_id,
                json!({
                    "entries": [
                        {"account": format!("wallet:{payer_id}:available"), "amount": -input.max_amount},
                        {"account": format!("wallet:{payer_id}:held"), "amount": input.max_amount}
                    ],
                    "quoteId": input.quote_id,
                    "pricingVersion": input.pricing_version,
                    "projectId": project_id,
                }),
                &participants,
                now,
            )?;

            Ok(json!({
                "applied": true,
                "hold": hold_map,
                "journalId": journal_id,
            }))
        },
    )
}

fn start_hold(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<StartHoldInput, _>(
        app,
        "/creatures/startHold",
        finance_guard(),
        move |state: Arc<dyn IState>, input: StartHoldInput| -> Result<Value> {
            let trx = state.trx();
            let authority_id = state.info().user_id();
            let now = Utc::now().timestamp_millis();
            if !valid_finance_id(&input.hold_id)
                || !valid_finance_id(&input.payer_user_id)
                || !valid_finance_id(&input.quote_id)
                || !valid_finance_id(&input.run_id)
            {
                return Err(anyhow!("invalid run authorization"));
            }

            let run_marker = format!("FinanceRun::{authority_id}::{}", input.run_id);
            let previous_hold_id = trx.get_link(&run_marker);
            if !previous_hold_id.is_empty() {
                if previous_hold_id != input.hold_id {
                    return Err(anyhow!("run id already used for another hold"));
                }
                let hold = get_finance_hold(&*trx, &input.hold_id)?;
                return Ok(json!({
                    "applied": false,
                    "alreadyApplied": true,
                    "hold": hold,
                }));
            }

            let mut hold = get_finance_hold(&*trx, &input.hold_id)?;
            if hold.get("status").and_then(Value::as_str) != Some("open") {
                return Err(anyhow!("hold is not open"));
            }
            if hold.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str())
            {
                return Err(anyhow!("payer does not match hold"));
            }
            if hold.get("quoteId").and_then(Value::as_str) != Some(input.quote_id.as_str()) {
                return Err(anyhow!("quote does not match hold"));
            }
            if hold.get("settlementAuthority").and_then(Value::as_str)
                != Some(authority_id.as_str())
            {
                return Err(anyhow!("caller is not the settlement authority"));
            }
            let expires_at = hold.get("expiresAt").and_then(as_i64).unwrap_or(0);
            if expires_at <= 0 || now > expires_at {
                return Err(anyhow!("hold expired"));
            }

            hold.insert("status".to_string(), json!("running"));
            hold.insert("runId".to_string(), json!(input.run_id));
            hold.insert("startedAt".to_string(), json!(now));
            put_finance_hold(&*trx, &input.hold_id, &hold)?;
            trx.put_link(&run_marker, &input.hold_id);
            let participants = vec![input.payer_user_id.clone(), authority_id];
            let journal_id = write_finance_journal(
                &*trx,
                "hold.started",
                &input.hold_id,
                &input.payer_user_id,
                json!({
                    "entries": [],
                    "quoteId": input.quote_id,
                    "runId": input.run_id,
                }),
                &participants,
                now,
            )?;
            Ok(json!({
                "applied": true,
                "hold": hold,
                "journalId": journal_id,
            }))
        },
    )
}

fn settle_hold(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<SettleHoldInput, _>(
        app,
        "/creatures/settleHold",
        finance_guard(),
        move |state: Arc<dyn IState>, input: SettleHoldInput| -> Result<Value> {
            let trx = state.trx();
            let authority_id = state.info().user_id();
            let now = Utc::now().timestamp_millis();

            if !valid_finance_id(&input.hold_id)
                || !valid_finance_id(&input.payer_user_id)
                || !valid_finance_id(&input.quote_id)
                || !valid_finance_id(&input.settlement_id)
                || !valid_finance_hash(&input.usage_hash)
            {
                return Err(anyhow!("invalid settlement identifiers or usageHash"));
            }
            let settlement_marker =
                format!("FinanceSettlement::{authority_id}::{}", input.settlement_id);
            let previous_hold_id = trx.get_link(&settlement_marker);
            if !previous_hold_id.is_empty() {
                if previous_hold_id != input.hold_id {
                    return Err(anyhow!("settlement id already used for another hold"));
                }
                let hold = get_finance_hold(&*trx, &input.hold_id)?;
                return Ok(json!({
                    "applied": false,
                    "alreadyApplied": true,
                    "hold": hold,
                }));
            }

            let mut hold = get_finance_hold(&*trx, &input.hold_id)?;
            if hold.get("status").and_then(Value::as_str) != Some("running") {
                return Err(anyhow!("hold is not running"));
            }
            if hold.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str())
            {
                return Err(anyhow!("payer does not match hold"));
            }
            if hold.get("quoteId").and_then(Value::as_str) != Some(input.quote_id.as_str()) {
                return Err(anyhow!("quote does not match hold"));
            }
            if hold.get("runId").and_then(Value::as_str) != Some(input.settlement_id.as_str()) {
                return Err(anyhow!("settlement does not match authorized run"));
            }
            if hold.get("settlementAuthority").and_then(Value::as_str)
                != Some(authority_id.as_str())
            {
                return Err(anyhow!("caller is not the settlement authority"));
            }
            let expires_at = hold.get("expiresAt").and_then(as_i64).unwrap_or(0);
            if expires_at <= 0 || now > expires_at {
                return Err(anyhow!("hold expired"));
            }
            let max_amount = hold.get("maxAmount").and_then(as_i64).unwrap_or(0);
            if max_amount <= 0 {
                return Err(anyhow!("invalid hold amount"));
            }

            let beneficiaries = hold
                .get("beneficiaries")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("hold beneficiaries missing"))?;
            let mut caps: HashMap<String, i64> = HashMap::new();
            for item in beneficiaries {
                let user_id = item.get("userId").and_then(Value::as_str).unwrap_or("");
                let role = item.get("role").and_then(Value::as_str).unwrap_or("");
                let cap = item.get("maxAmount").and_then(as_i64).unwrap_or(0);
                if user_id.is_empty() || role.is_empty() || cap <= 0 {
                    return Err(anyhow!("invalid hold beneficiary"));
                }
                caps.insert(format!("{user_id}|{role}"), cap);
            }

            let mut actual_amount = 0_i64;
            let mut allocated: HashMap<String, i64> = HashMap::new();
            let mut credits: HashMap<String, i64> = HashMap::new();
            for line in &input.lines {
                if line.amount <= 0
                    || !valid_finance_id(&line.user_id)
                    || !valid_finance_id(&line.role)
                    || line.source_ref.len() > 256
                {
                    return Err(anyhow!("invalid settlement line"));
                }
                let cap_key = format!("{}|{}", line.user_id, line.role);
                let Some(cap) = caps.get(&cap_key) else {
                    return Err(anyhow!(
                        "settlement beneficiary role not authorized by hold"
                    ));
                };
                actual_amount = actual_amount
                    .checked_add(line.amount)
                    .ok_or_else(|| anyhow!("settlement amount overflow"))?;
                let role_total = allocated.entry(cap_key).or_insert(0);
                *role_total = role_total
                    .checked_add(line.amount)
                    .ok_or_else(|| anyhow!("beneficiary role amount overflow"))?;
                if *role_total > *cap {
                    return Err(anyhow!("settlement exceeds beneficiary role cap"));
                }
                let credited = credits.entry(line.user_id.clone()).or_insert(0);
                *credited = credited
                    .checked_add(line.amount)
                    .ok_or_else(|| anyhow!("beneficiary amount overflow"))?;
            }
            if actual_amount > max_amount {
                return Err(anyhow!("settlement exceeds hold"));
            }
            let refund_amount = max_amount
                .checked_sub(actual_amount)
                .ok_or_else(|| anyhow!("refund underflow"))?;
            let project_id = hold
                .get("projectId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            finalize_project_budget(&*trx, &project_id, max_amount, actual_amount, now)?;

            add_finance_counter(
                &*trx,
                &format!("FinanceSpent::{}", input.payer_user_id),
                actual_amount,
            )?;
            let mut participants = vec![input.payer_user_id.clone(), authority_id.clone()];
            let mut wallet_credits: HashMap<String, i64> = HashMap::new();
            let mut debt_repays: HashMap<String, i64> = HashMap::new();
            for (user_id, amount) in &credits {
                add_finance_counter(&*trx, &format!("FinanceEarned::{user_id}"), *amount)?;
                let Some(mut receiver) =
                    (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                        .account(&user_id.clone())?
                else {
                    return Err(anyhow!("settlement beneficiary not found"));
                };
                let debt = finance_debt_amount(&*trx, user_id)?;
                let debt_repaid = debt.min(*amount);
                let wallet_credit = amount
                    .checked_sub(debt_repaid)
                    .ok_or_else(|| anyhow!("beneficiary credit underflow"))?;
                receiver.balance = receiver
                    .balance
                    .checked_add(wallet_credit)
                    .ok_or_else(|| anyhow!("beneficiary balance overflow"))?;
                let withdrawable = finance_withdrawable_amount(&*trx, user_id)?
                    .checked_add(wallet_credit)
                    .ok_or_else(|| anyhow!("withdrawable earnings overflow"))?;
                set_finance_debt_amount(&*trx, user_id, debt - debt_repaid)?;
                set_finance_withdrawable_amount(&*trx, user_id, withdrawable)?;
                wallet_credits.insert(user_id.clone(), wallet_credit);
                debt_repays.insert(user_id.clone(), debt_repaid);
                (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .store_account(&receiver)?;
                participants.push(user_id.clone());
            }

            let Some(mut payer) =
                (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .account(&input.payer_user_id.clone())?
            else {
                return Err(anyhow!("payer creature not found"));
            };
            payer.balance = payer
                .balance
                .checked_add(refund_amount)
                .ok_or_else(|| anyhow!("payer balance overflow"))?;
            let held_withdrawable = hold.get("withdrawableAmount").and_then(as_i64).unwrap_or(0);
            let withdrawable_refund = refund_amount.min(held_withdrawable);
            let withdrawable_spent = held_withdrawable
                .checked_sub(withdrawable_refund)
                .ok_or_else(|| anyhow!("withdrawable settlement underflow"))?;
            let payer_withdrawable = finance_withdrawable_amount(&*trx, &input.payer_user_id)?
                .checked_add(withdrawable_refund)
                .ok_or_else(|| anyhow!("withdrawable refund overflow"))?;
            set_finance_withdrawable_amount(&*trx, &input.payer_user_id, payer_withdrawable)?;
            (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                .store_account(&payer)?;
            let held = finance_held_amount(&*trx, &input.payer_user_id)?
                .checked_sub(max_amount)
                .ok_or_else(|| anyhow!("held balance underflow"))?;
            set_finance_held_amount(&*trx, &input.payer_user_id, held)?;

            hold.insert("status".to_string(), json!("settled"));
            hold.insert("remainingAmount".to_string(), json!(0));
            hold.insert("actualAmount".to_string(), json!(actual_amount));
            hold.insert("refundedAmount".to_string(), json!(refund_amount));
            hold.insert(
                "withdrawableRefundedAmount".to_string(),
                json!(withdrawable_refund),
            );
            hold.insert(
                "withdrawableSpentAmount".to_string(),
                json!(withdrawable_spent),
            );
            hold.insert("settlementId".to_string(), json!(input.settlement_id));
            hold.insert("usageHash".to_string(), json!(input.usage_hash));
            hold.insert(
                "settlementLines".to_string(),
                serde_json::to_value(&input.lines)?,
            );
            hold.insert("finalizedAt".to_string(), json!(now));
            put_finance_hold(&*trx, &input.hold_id, &hold)?;
            trx.put_link(&settlement_marker, &input.hold_id);

            let mut entries = vec![
                json!({
                    "account": format!("wallet:{}:held", input.payer_user_id),
                    "amount": -max_amount,
                }),
                json!({
                    "account": format!("wallet:{}:available", input.payer_user_id),
                    "amount": refund_amount,
                }),
            ];
            for (user_id, gross_amount) in &credits {
                let wallet_credit = wallet_credits.get(user_id).copied().unwrap_or(0);
                let debt_repaid = debt_repays.get(user_id).copied().unwrap_or(0);
                entries.push(json!({
                    "account": format!("wallet:{user_id}:available"),
                    "amount": wallet_credit,
                    "grossAmount": gross_amount,
                }));
                if debt_repaid > 0 {
                    entries.push(json!({
                        "account": format!("wallet:{user_id}:debt"),
                        "amount": -debt_repaid,
                    }));
                }
            }
            let journal_id = write_finance_journal(
                &*trx,
                "hold.settled",
                &input.hold_id,
                &input.payer_user_id,
                json!({
                    "entries": entries,
                    "quoteId": input.quote_id,
                    "settlementId": input.settlement_id,
                    "usageHash": input.usage_hash,
                    "actualAmount": actual_amount,
                    "refundedAmount": refund_amount,
                    "settlementLines": input.lines,
                }),
                &participants,
                now,
            )?;

            Ok(json!({
                "applied": true,
                "hold": hold,
                "journalId": journal_id,
            }))
        },
    )
}

fn release_hold(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ReleaseHoldInput, _>(
        app,
        "/creatures/releaseHold",
        finance_guard(),
        move |state: Arc<dyn IState>, input: ReleaseHoldInput| -> Result<Value> {
            let trx = state.trx();
            let caller_id = state.info().user_id();
            let now = Utc::now().timestamp_millis();

            if !valid_finance_id(&input.hold_id)
                || !valid_finance_id(&input.payer_user_id)
                || !valid_finance_id(&input.release_id)
                || input.reason.len() > 256
            {
                return Err(anyhow!("invalid release request"));
            }
            let release_marker = format!("FinanceRelease::{caller_id}::{}", input.release_id);
            let previous_hold_id = trx.get_link(&release_marker);
            if !previous_hold_id.is_empty() {
                if previous_hold_id != input.hold_id {
                    return Err(anyhow!("release id already used for another hold"));
                }
                let hold = get_finance_hold(&*trx, &input.hold_id)?;
                return Ok(json!({
                    "applied": false,
                    "alreadyApplied": true,
                    "hold": hold,
                }));
            }

            let mut hold = get_finance_hold(&*trx, &input.hold_id)?;
            let active_status = hold.get("status").and_then(Value::as_str).unwrap_or("");
            if active_status != "open" && active_status != "running" {
                return Err(anyhow!("hold is not active"));
            }
            if hold.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str())
            {
                return Err(anyhow!("payer does not match hold"));
            }
            let authority = hold
                .get("settlementAuthority")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let expires_at = hold.get("expiresAt").and_then(as_i64).unwrap_or(0);
            let payer_open_release = caller_id == input.payer_user_id && active_status == "open";
            let payer_expired_release = caller_id == input.payer_user_id && now >= expires_at;
            if caller_id != authority && !payer_open_release && !payer_expired_release {
                return Err(anyhow!("only the authority may release an active hold"));
            }
            let max_amount = hold.get("maxAmount").and_then(as_i64).unwrap_or(0);
            if max_amount <= 0 {
                return Err(anyhow!("invalid hold amount"));
            }
            let project_id = hold
                .get("projectId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            finalize_project_budget(&*trx, &project_id, max_amount, 0, now)?;

            let Some(mut payer) =
                (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .account(&input.payer_user_id.clone())?
            else {
                return Err(anyhow!("payer creature not found"));
            };
            payer.balance = payer
                .balance
                .checked_add(max_amount)
                .ok_or_else(|| anyhow!("payer balance overflow"))?;
            let withdrawable_refund = hold.get("withdrawableAmount").and_then(as_i64).unwrap_or(0);
            let withdrawable = finance_withdrawable_amount(&*trx, &input.payer_user_id)?
                .checked_add(withdrawable_refund)
                .ok_or_else(|| anyhow!("withdrawable refund overflow"))?;
            set_finance_withdrawable_amount(&*trx, &input.payer_user_id, withdrawable)?;
            (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                .store_account(&payer)?;
            let held = finance_held_amount(&*trx, &input.payer_user_id)?
                .checked_sub(max_amount)
                .ok_or_else(|| anyhow!("held balance underflow"))?;
            set_finance_held_amount(&*trx, &input.payer_user_id, held)?;

            let status = if now >= expires_at {
                "expired"
            } else {
                "released"
            };
            hold.insert("status".to_string(), json!(status));
            hold.insert("remainingAmount".to_string(), json!(0));
            hold.insert("refundedAmount".to_string(), json!(max_amount));
            hold.insert(
                "withdrawableRefundedAmount".to_string(),
                json!(withdrawable_refund),
            );
            hold.insert("releaseId".to_string(), json!(input.release_id));
            hold.insert("releaseReason".to_string(), json!(input.reason));
            hold.insert("finalizedAt".to_string(), json!(now));
            put_finance_hold(&*trx, &input.hold_id, &hold)?;
            trx.put_link(&release_marker, &input.hold_id);

            let participants = vec![input.payer_user_id.clone(), authority];
            let journal_id = write_finance_journal(
                &*trx,
                "hold.released",
                &input.hold_id,
                &input.payer_user_id,
                json!({
                    "entries": [
                        {
                            "account": format!("wallet:{}:held", input.payer_user_id),
                            "amount": -max_amount,
                        },
                        {
                            "account": format!("wallet:{}:available", input.payer_user_id),
                            "amount": max_amount,
                        }
                    ],
                    "status": status,
                    "reason": input.reason,
                }),
                &participants,
                now,
            )?;

            Ok(json!({
                "applied": true,
                "hold": hold,
                "journalId": journal_id,
            }))
        },
    )
}

fn get_hold(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<GetHoldInput, _>(
        app,
        "/creatures/getHold",
        finance_guard(),
        move |state: Arc<dyn IState>, input: GetHoldInput| -> Result<Value> {
            if !valid_finance_id(&input.hold_id) {
                return Err(anyhow!("invalid hold id"));
            }
            let hold = get_finance_hold(&*state.trx(), &input.hold_id)?;
            let payer_id = hold
                .get("payerUserId")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !input.payer_user_id.is_empty() && input.payer_user_id != payer_id {
                return Err(anyhow!("payer does not match hold"));
            }
            let caller_id = state.info().user_id();
            let authority = hold
                .get("settlementAuthority")
                .and_then(Value::as_str)
                .unwrap_or("");
            let is_beneficiary = hold
                .get("beneficiaries")
                .and_then(Value::as_array)
                .map(|items| {
                    items.iter().any(|item| {
                        item.get("userId").and_then(Value::as_str) == Some(caller_id.as_str())
                    })
                })
                .unwrap_or(false);
            if caller_id != payer_id && caller_id != authority && !is_beneficiary {
                return Err(anyhow!("access denied"));
            }
            Ok(json!({"hold": hold}))
        },
    )
}

fn finance_payout_key(payout_id: &str) -> String {
    format!("Json::FinancePayout::{payout_id}")
}

fn get_finance_payout(trx: &dyn ITrx, payout_id: &str) -> Result<Map<String, Value>> {
    trx.get_json(&finance_payout_key(payout_id), "payout")
        .map_err(|_| anyhow!("payout not found"))
}

fn finance_payout_records(trx: &dyn ITrx, user_id: &str, limit: usize) -> Vec<Value> {
    let mut keys = trx
        .get_links_list(&format!("FinancePayoutByUser::{user_id}::"), -1, -1, &[])
        .unwrap_or_default();
    keys.sort();
    keys.reverse();
    let mut payouts = Vec::new();
    for key in keys.into_iter().take(limit) {
        let payout_id = trx.get_link(&key);
        if let Ok(payout) = get_finance_payout(trx, &payout_id) {
            payouts.push(Value::Object(payout));
        }
    }
    payouts
}

fn financial_account_snapshot(trx: &dyn ITrx, user_id: &str, limit: usize) -> Result<Value> {
    let Some(creature) = (crate::shell::api::model::creature_ports::LegacyCreatures { trx })
        .account(&user_id.to_string())?
    else {
        return Err(anyhow!("financial account not found"));
    };
    let mut journal_keys = trx
        .get_links_list(&format!("FinanceJournalByUser::{user_id}::"), -1, -1, &[])
        .unwrap_or_default();
    journal_keys.sort();
    journal_keys.reverse();
    let mut transactions = Vec::new();
    for key in journal_keys.into_iter().take(limit) {
        let journal_id = trx.get_link(&key);
        if !journal_id.is_empty() {
            if let Ok(entry) = trx.get_json(&format!("Json::FinanceJournal::{journal_id}"), "entry")
            {
                transactions.push(Value::Object(entry));
            }
        }
    }
    let mut hold_keys = trx
        .get_links_list(&format!("FinanceHoldByPayer::{user_id}::"), -1, -1, &[])
        .unwrap_or_default();
    hold_keys.sort();
    hold_keys.reverse();
    let mut active_holds = Vec::new();
    for key in hold_keys.into_iter().take(100) {
        let hold_id = trx.get_link(&key);
        if let Ok(hold) = get_finance_hold(trx, &hold_id) {
            let status = hold.get("status").and_then(Value::as_str).unwrap_or("");
            if status == "open" || status == "running" {
                active_holds.push(Value::Object(hold));
            }
        }
    }
    // The user's shared authorization pool (if any), so the client can show what
    // is pooled for runs (remaining/reserved/spent) instead of an opaque "held".
    let pool = {
        let pool_id = trx.get_link(&format!("FinancePoolByUser::{user_id}"));
        if pool_id.is_empty() {
            Value::Null
        } else {
            match trx.get_json(&finance_pool_key(&pool_id), "pool") {
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
        "heldMinor": finance_held_amount(trx, user_id)?,
        "debtMinor": finance_debt_amount(trx, user_id)?,
        "withdrawableMinor": finance_withdrawable_amount(trx, user_id)?,
        "payoutHeldMinor": finance_payout_held_amount(trx, user_id)?,
        "earnedMinor": finance_counter(trx, &format!("FinanceEarned::{user_id}"))?,
        "spentMinor": finance_counter(trx, &format!("FinanceSpent::{user_id}"))?,
        "activeHolds": active_holds,
        "pool": pool,
        "transactions": transactions,
        "payouts": finance_payout_records(trx, user_id, limit),
    }))
}

fn get_financial_account(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<GetFinancialAccountInput, _>(
        app,
        "/creatures/getFinancialAccount",
        finance_guard(),
        move |state: Arc<dyn IState>, input: GetFinancialAccountInput| -> Result<Value> {
            let caller_id = state.info().user_id();
            let user_id = if input.user_id.is_empty() {
                caller_id.clone()
            } else {
                input.user_id
            };
            if !valid_finance_id(&user_id) {
                return Err(anyhow!("invalid financial account id"));
            }
            if user_id != caller_id && caller_id != "1@global" {
                return Err(anyhow!("access denied"));
            }
            let limit = if input.limit <= 0 {
                50
            } else {
                input.limit.min(100) as usize
            };
            financial_account_snapshot(&*state.trx(), &user_id, limit)
        },
    )
}

fn finance_map_add(totals: &mut HashMap<String, i64>, key: &str, amount: i64) -> bool {
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

fn request_payout(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<RequestPayoutInput, _>(
        app,
        "/creatures/requestPayout",
        finance_guard(),
        move |state: Arc<dyn IState>, input: RequestPayoutInput| -> Result<Value> {
            let user_id = state.info().user_id();
            let destination = input.destination_ref.trim();
            if !valid_finance_id(&input.request_id)
                || input.amount <= 0
                || destination.is_empty()
                || destination.len() > 256
                || destination.chars().any(char::is_control)
            {
                return Err(anyhow!("invalid payout request"));
            }
            let trx = state.trx();
            let request_hash = finance_hash(&serde_json::to_value(&input)?)?;
            let marker = format!("FinancePayoutRequest::{user_id}::{}", input.request_id);
            let previous = trx.get_link(&marker);
            if !previous.is_empty() {
                let Some((payout_id, previous_hash)) = previous.split_once(char::from(124)) else {
                    return Err(anyhow!("invalid payout idempotency record"));
                };
                if previous_hash != request_hash {
                    return Err(anyhow!("requestId already used with different payout data"));
                }
                return Ok(json!({
                    "applied": false,
                    "alreadyApplied": true,
                    "payout": get_finance_payout(&*trx, payout_id)?,
                }));
            }
            if finance_debt_amount(&*trx, &user_id)? > 0 {
                return Err(anyhow!("wallet has outstanding payment debt"));
            }
            let Some(mut creature) =
                (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .account(&user_id.clone())?
            else {
                return Err(anyhow!("financial account not found"));
            };
            let withdrawable = finance_withdrawable_amount(&*trx, &user_id)?;
            if input.amount > withdrawable || input.amount > creature.balance {
                return Err(anyhow!("withdrawable earnings are not enough"));
            }
            creature.balance = creature
                .balance
                .checked_sub(input.amount)
                .ok_or_else(|| anyhow!("wallet payout underflow"))?;
            let next_withdrawable = withdrawable
                .checked_sub(input.amount)
                .ok_or_else(|| anyhow!("withdrawable payout underflow"))?;
            let payout_held = finance_payout_held_amount(&*trx, &user_id)?
                .checked_add(input.amount)
                .ok_or_else(|| anyhow!("payout held overflow"))?;
            let now = Utc::now().timestamp_millis();
            let payout_id = secure_unique_string();
            let payout = json!({
                "payoutId": payout_id,
                "requestId": input.request_id,
                "userId": user_id,
                "amount": input.amount,
                "destinationRef": destination,
                "status": "pending",
                "createdAt": now,
                "requestHash": request_hash,
            });
            let payout_map = payout
                .as_object()
                .cloned()
                .ok_or_else(|| anyhow!("invalid payout record"))?;
            (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                .store_account(&creature)?;
            set_finance_withdrawable_amount(&*trx, &user_id, next_withdrawable)?;
            set_finance_payout_held_amount(&*trx, &user_id, payout_held)?;
            trx.put_json(&finance_payout_key(&payout_id), "payout", &payout, false)?;
            trx.put_link(
                &marker,
                &format!("{}{}{}", payout_id, char::from(124), request_hash),
            );
            trx.put_link(
                &format!("FinancePayoutByUser::{user_id}::{now:020}::{payout_id}"),
                &payout_id,
            );
            let journal_id = write_finance_journal(
                &*trx,
                "payout.requested",
                "",
                &user_id,
                json!({
                    "entries": [
                        {"account": format!("wallet:{user_id}:available"), "amount": -input.amount},
                        {"account": format!("wallet:{user_id}:payout_held"), "amount": input.amount}
                    ],
                    "payoutId": payout_id,
                    "destinationRef": destination,
                }),
                std::slice::from_ref(&user_id),
                now,
            )?;
            Ok(json!({"applied": true, "payout": payout_map, "journalId": journal_id}))
        },
    )
}

fn resolve_payout(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ResolvePayoutInput, _>(
        app,
        "/creatures/resolvePayout",
        finance_guard(),
        move |state: Arc<dyn IState>, input: ResolvePayoutInput| -> Result<Value> {
            if state.info().user_id() != "1@global" {
                return Err(anyhow!("access denied"));
            }
            if !valid_finance_id(&input.payout_id)
                || !valid_finance_id(&input.resolution_id)
                || (input.status != "paid" && input.status != "rejected")
                || input.provider_reference.len() > 256
                || input.provider_reference.chars().any(char::is_control)
                || input.reason.len() > 256
                || input.reason.chars().any(char::is_control)
                || (input.status == "paid" && input.provider_reference.trim().is_empty())
            {
                return Err(anyhow!("invalid payout resolution"));
            }
            let trx = state.trx();
            let request_hash = finance_hash(&serde_json::to_value(&input)?)?;
            let marker = format!("FinancePayoutResolution::{}", input.resolution_id);
            let previous = trx.get_link(&marker);
            if !previous.is_empty() {
                let Some((payout_id, previous_hash)) = previous.split_once(char::from(124)) else {
                    return Err(anyhow!("invalid payout resolution idempotency record"));
                };
                if payout_id != input.payout_id || previous_hash != request_hash {
                    return Err(anyhow!(
                        "resolutionId already used with different payout data"
                    ));
                }
                return Ok(json!({
                    "applied": false,
                    "alreadyApplied": true,
                    "payout": get_finance_payout(&*trx, payout_id)?,
                }));
            }
            let mut payout = get_finance_payout(&*trx, &input.payout_id)?;
            if payout.get("status").and_then(Value::as_str) != Some("pending") {
                return Err(anyhow!("payout is not pending"));
            }
            let user_id = payout
                .get("userId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let amount = payout.get("amount").and_then(as_i64).unwrap_or(0);
            if user_id.is_empty() || amount <= 0 {
                return Err(anyhow!("invalid payout record"));
            }
            let payout_held = finance_payout_held_amount(&*trx, &user_id)?
                .checked_sub(amount)
                .ok_or_else(|| anyhow!("payout held underflow"))?;
            set_finance_payout_held_amount(&*trx, &user_id, payout_held)?;
            let mut entries = vec![json!({
                "account": format!("wallet:{user_id}:payout_held"),
                "amount": -amount,
            })];
            if input.status == "rejected" {
                let Some(mut creature) =
                    (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                        .account(&user_id.clone())?
                else {
                    return Err(anyhow!("payout owner not found"));
                };
                creature.balance = creature
                    .balance
                    .checked_add(amount)
                    .ok_or_else(|| anyhow!("payout refund overflow"))?;
                let withdrawable = finance_withdrawable_amount(&*trx, &user_id)?
                    .checked_add(amount)
                    .ok_or_else(|| anyhow!("withdrawable payout refund overflow"))?;
                (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .store_account(&creature)?;
                set_finance_withdrawable_amount(&*trx, &user_id, withdrawable)?;
                entries.push(
                    json!({"account": format!("wallet:{user_id}:available"), "amount": amount}),
                );
            } else {
                entries.push(json!({"account": "external:payouts", "amount": amount}));
            }
            let now = Utc::now().timestamp_millis();
            payout.insert("status".to_string(), json!(input.status));
            payout.insert(
                "providerReference".to_string(),
                json!(input.provider_reference),
            );
            payout.insert("reason".to_string(), json!(input.reason));
            payout.insert("resolutionId".to_string(), json!(input.resolution_id));
            payout.insert("resolvedAt".to_string(), json!(now));
            trx.put_json(
                &finance_payout_key(&input.payout_id),
                "payout",
                &Value::Object(payout.clone()),
                false,
            )?;
            trx.put_link(
                &marker,
                &format!("{}{}{}", input.payout_id, char::from(124), request_hash),
            );
            let participants = vec![user_id.clone(), state.info().user_id()];
            let journal_id = write_finance_journal(
                &*trx,
                &format!("payout.{}", input.status),
                "",
                &user_id,
                json!({
                    "entries": entries,
                    "payoutId": input.payout_id,
                    "providerReference": input.provider_reference,
                    "reason": input.reason,
                }),
                &participants,
                now,
            )?;
            Ok(json!({"applied": true, "payout": payout, "journalId": journal_id}))
        },
    )
}

fn list_payouts(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ListPayoutsInput, _>(
        app,
        "/creatures/listPayouts",
        finance_guard(),
        move |state: Arc<dyn IState>, input: ListPayoutsInput| -> Result<Value> {
            let caller = state.info().user_id();
            let limit = if input.limit <= 0 {
                50_usize
            } else {
                input.limit.min(200) as usize
            };
            if input.user_id.is_empty() {
                if caller != "1@global" {
                    return Ok(
                        json!({"payouts": finance_payout_records(&*state.trx(), &caller, limit)}),
                    );
                }
                let trx = state.trx();
                let prefix = "json::Json::FinancePayout::";
                let mut payouts: Vec<Value> = Vec::new();
                for key in trx.get_by_prefix(prefix) {
                    let Some(payout_id) = key
                        .strip_prefix(prefix)
                        .and_then(|rest| rest.strip_suffix("::payout"))
                    else {
                        continue;
                    };
                    if let Ok(payout) = get_finance_payout(&*trx, payout_id) {
                        payouts.push(Value::Object(payout));
                    }
                }
                payouts.sort_by(|a, b| {
                    b.get("createdAt")
                        .and_then(as_i64)
                        .unwrap_or(0)
                        .cmp(&a.get("createdAt").and_then(as_i64).unwrap_or(0))
                });
                payouts.truncate(limit);
                return Ok(json!({"payouts": payouts}));
            }
            if input.user_id != caller && caller != "1@global" {
                return Err(anyhow!("access denied"));
            }
            Ok(json!({
                "payouts": finance_payout_records(&*state.trx(), &input.user_id, limit),
            }))
        },
    )
}

// ── Shared authorization pool ────────────────────────────────────────────────
// A per-user pool is a standing hold many runs draw down together (see
// decillionai-server/docs/SHARED-POOL-DESIGN.md). Wallet accounting mirrors
// createHold/releaseHold; only the granularity differs (one long-lived hold with
// a running `remaining`/`reserved`/`spent` instead of one hold per run). Every
// transition keeps the invariant maxAmount == remaining + reserved + spent +
// refunded, and payer `held` == pool `remaining + reserved`.

/// Split a wallet debit into the withdrawable portion it must consume, given the
/// payer's current balance and withdrawable counter. Non-withdrawable (top-up)
/// funds are consumed first. Returns the withdrawable amount to deduct.
fn withdrawable_debit_portion(balance: i64, withdrawable: i64, amount: i64) -> i64 {
    let nonwithdrawable = balance.saturating_sub(withdrawable);
    amount.saturating_sub(nonwithdrawable)
}

fn open_pool(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<OpenPoolInput, _>(
        app,
        "/creatures/openPool",
        finance_guard(),
        move |state: Arc<dyn IState>, input: OpenPoolInput| -> Result<Value> {
            let trx = state.trx();
            let payer_id = state.info().user_id();
            let now = Utc::now().timestamp_millis();
            if !valid_finance_id(&input.settlement_authority)
                || !valid_finance_id(&input.meter_program_id)
                || !valid_finance_id(&input.idempotency_key)
            {
                return Err(anyhow!(
                    "invalid authority, meter, or idempotency identifier"
                ));
            }
            if input.max_amount <= 0 {
                return Err(anyhow!("maxAmount must be greater than zero"));
            }
            if input.expires_at <= now {
                return Err(anyhow!("pool expiry must be in the future"));
            }
            let marker = format!("FinancePoolOpen::{payer_id}::{}", input.idempotency_key);
            let existing = trx.get_link(&marker);
            if !existing.is_empty() {
                let pool = get_finance_pool(&*trx, &existing)?;
                return Ok(json!({"applied": false, "alreadyApplied": true, "pool": pool}));
            }
            if finance_debt_amount(&*trx, &payer_id)? > 0 {
                return Err(anyhow!("wallet has outstanding payment debt"));
            }
            let Some(mut payer) =
                (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .account(&payer_id.clone())?
            else {
                return Err(anyhow!("payer creature not found"));
            };
            let withdrawable = finance_withdrawable_amount(&*trx, &payer_id)?;
            if withdrawable > payer.balance {
                return Err(anyhow!("withdrawable balance exceeds available balance"));
            }
            let withdrawable_amount =
                withdrawable_debit_portion(payer.balance, withdrawable, input.max_amount);
            payer.balance = payer
                .balance
                .checked_sub(input.max_amount)
                .ok_or_else(|| anyhow!("insufficient available balance to open this pool"))?;
            set_finance_withdrawable_amount(
                &*trx,
                &payer_id,
                withdrawable
                    .checked_sub(withdrawable_amount)
                    .ok_or_else(|| anyhow!("withdrawable composition underflow"))?,
            )?;
            (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                .store_account(&payer)?;
            let held = finance_held_amount(&*trx, &payer_id)?
                .checked_add(input.max_amount)
                .ok_or_else(|| anyhow!("held balance overflow"))?;
            set_finance_held_amount(&*trx, &payer_id, held)?;

            let pool_id = secure_unique_string();
            let pool = json!({
                "version": 1,
                "poolId": pool_id,
                "payerUserId": payer_id,
                "maxAmount": input.max_amount,
                "remaining": input.max_amount,
                "reserved": 0,
                "spent": 0,
                "refunded": 0,
                "withdrawableAmount": withdrawable_amount,
                "settlementAuthority": input.settlement_authority,
                "meterProgramId": input.meter_program_id,
                "status": "open",
                "expiresAt": input.expires_at,
                "idempotencyKey": input.idempotency_key,
                "createdAt": now,
                "updatedAt": now,
            });
            let pool_map = pool
                .as_object()
                .cloned()
                .ok_or_else(|| anyhow!("pool encode failed"))?;
            put_finance_pool(&*trx, &pool_id, &pool_map)?;
            trx.put_link(&marker, &pool_id);
            // One index link per user to the current pool, so a private account read
            // can surface it without the caller tracking the id.
            trx.put_link(&format!("FinancePoolByUser::{payer_id}"), &pool_id);
            let participants = vec![payer_id.clone(), input.settlement_authority.clone()];
            let journal_id = write_finance_journal(
                &*trx,
                "pool.opened",
                &pool_id,
                &payer_id,
                json!({"maxAmount": input.max_amount, "withdrawableAmount": withdrawable_amount}),
                &participants,
                now,
            )?;
            Ok(json!({"applied": true, "pool": pool, "journalId": journal_id}))
        },
    )
}

fn refresh_pool(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<RefreshPoolInput, _>(
        app,
        "/creatures/refreshPool",
        finance_guard(),
        move |state: Arc<dyn IState>, input: RefreshPoolInput| -> Result<Value> {
            let trx = state.trx();
            let payer_id = state.info().user_id();
            let now = Utc::now().timestamp_millis();
            if !valid_finance_id(&input.pool_id) || !valid_finance_id(&input.refresh_id) {
                return Err(anyhow!("invalid pool or refresh identifier"));
            }
            if input.amount <= 0 {
                return Err(anyhow!("refresh amount must be greater than zero"));
            }
            let marker = format!("FinancePoolRefresh::{payer_id}::{}", input.refresh_id);
            if !trx.get_link(&marker).is_empty() {
                let pool = get_finance_pool(&*trx, &input.pool_id)?;
                return Ok(json!({"applied": false, "alreadyApplied": true, "pool": pool}));
            }
            if finance_debt_amount(&*trx, &payer_id)? > 0 {
                return Err(anyhow!("wallet has outstanding payment debt"));
            }
            let mut pool = get_finance_pool(&*trx, &input.pool_id)?;
            if pool.get("payerUserId").and_then(Value::as_str) != Some(payer_id.as_str()) {
                return Err(anyhow!("pool does not belong to caller"));
            }
            if pool.get("status").and_then(Value::as_str) != Some("open") {
                return Err(anyhow!("pool is not open"));
            }
            let Some(mut payer) =
                (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .account(&payer_id.clone())?
            else {
                return Err(anyhow!("payer creature not found"));
            };
            let withdrawable = finance_withdrawable_amount(&*trx, &payer_id)?;
            if withdrawable > payer.balance {
                return Err(anyhow!("withdrawable balance exceeds available balance"));
            }
            let withdrawable_add =
                withdrawable_debit_portion(payer.balance, withdrawable, input.amount);
            payer.balance = payer
                .balance
                .checked_sub(input.amount)
                .ok_or_else(|| anyhow!("insufficient available balance to refresh this pool"))?;
            set_finance_withdrawable_amount(
                &*trx,
                &payer_id,
                withdrawable
                    .checked_sub(withdrawable_add)
                    .ok_or_else(|| anyhow!("withdrawable composition underflow"))?,
            )?;
            (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                .store_account(&payer)?;
            let held = finance_held_amount(&*trx, &payer_id)?
                .checked_add(input.amount)
                .ok_or_else(|| anyhow!("held balance overflow"))?;
            set_finance_held_amount(&*trx, &payer_id, held)?;

            let max_amount = pool
                .get("maxAmount")
                .and_then(as_i64)
                .unwrap_or(0)
                .checked_add(input.amount)
                .ok_or_else(|| anyhow!("pool maxAmount overflow"))?;
            let remaining = pool
                .get("remaining")
                .and_then(as_i64)
                .unwrap_or(0)
                .checked_add(input.amount)
                .ok_or_else(|| anyhow!("pool remaining overflow"))?;
            let pool_withdrawable = pool
                .get("withdrawableAmount")
                .and_then(as_i64)
                .unwrap_or(0)
                .checked_add(withdrawable_add)
                .ok_or_else(|| anyhow!("pool withdrawable overflow"))?;
            if input.expires_at > now {
                pool.insert("expiresAt".to_string(), json!(input.expires_at));
            }
            pool.insert("maxAmount".to_string(), json!(max_amount));
            pool.insert("remaining".to_string(), json!(remaining));
            pool.insert("withdrawableAmount".to_string(), json!(pool_withdrawable));
            pool.insert("updatedAt".to_string(), json!(now));
            put_finance_pool(&*trx, &input.pool_id, &pool)?;
            trx.put_link(&marker, &input.pool_id);
            let authority = pool
                .get("settlementAuthority")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let participants = vec![payer_id.clone(), authority];
            let journal_id = write_finance_journal(
                &*trx,
                "pool.refreshed",
                &input.pool_id,
                &payer_id,
                json!({"amount": input.amount, "maxAmount": max_amount}),
                &participants,
                now,
            )?;
            Ok(json!({"applied": true, "pool": Value::Object(pool), "journalId": journal_id}))
        },
    )
}

fn close_pool(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ClosePoolInput, _>(
        app,
        "/creatures/closePool",
        finance_guard(),
        move |state: Arc<dyn IState>, input: ClosePoolInput| -> Result<Value> {
            let trx = state.trx();
            let caller_id = state.info().user_id();
            let now = Utc::now().timestamp_millis();
            if !valid_finance_id(&input.pool_id) || !valid_finance_id(&input.close_id) {
                return Err(anyhow!("invalid pool or close identifier"));
            }
            let mut pool = get_finance_pool(&*trx, &input.pool_id)?;
            let payer_id = pool
                .get("payerUserId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let authority = pool
                .get("settlementAuthority")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // The payer may always close their own pool; the settlement authority
            // may close it too (e.g. teardown). No one else.
            if caller_id != payer_id && caller_id != authority {
                return Err(anyhow!("caller may not close this pool"));
            }
            let status = pool.get("status").and_then(Value::as_str).unwrap_or("");
            let close_marker = format!("FinancePoolClose::{}", input.pool_id);
            if status == "closed" {
                if trx.get_link(&close_marker) == input.close_id {
                    return Ok(
                        json!({"applied": false, "alreadyApplied": true, "pool": Value::Object(pool)}),
                    );
                }
                return Err(anyhow!("pool is already closed"));
            }
            if status != "open" {
                return Err(anyhow!("pool is not open"));
            }
            let reserved = pool.get("reserved").and_then(as_i64).unwrap_or(0);
            if reserved != 0 {
                return Err(anyhow!(
                    "pool has in-flight run reservations; cannot close yet"
                ));
            }
            let remaining = pool.get("remaining").and_then(as_i64).unwrap_or(0);
            let pool_withdrawable = pool.get("withdrawableAmount").and_then(as_i64).unwrap_or(0);
            let withdrawable_refund = remaining.min(pool_withdrawable);
            if remaining > 0 {
                let Some(mut payer) =
                    (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                        .account(&payer_id.clone())?
                else {
                    return Err(anyhow!("payer creature not found"));
                };
                payer.balance = payer
                    .balance
                    .checked_add(remaining)
                    .ok_or_else(|| anyhow!("payer balance overflow"))?;
                let withdrawable = finance_withdrawable_amount(&*trx, &payer_id)?
                    .checked_add(withdrawable_refund)
                    .ok_or_else(|| anyhow!("withdrawable refund overflow"))?;
                set_finance_withdrawable_amount(&*trx, &payer_id, withdrawable)?;
                (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .store_account(&payer)?;
                let held = finance_held_amount(&*trx, &payer_id)?
                    .checked_sub(remaining)
                    .ok_or_else(|| anyhow!("held balance underflow"))?;
                set_finance_held_amount(&*trx, &payer_id, held)?;
            }
            let refunded = pool
                .get("refunded")
                .and_then(as_i64)
                .unwrap_or(0)
                .checked_add(remaining)
                .ok_or_else(|| anyhow!("pool refunded overflow"))?;
            pool.insert("status".to_string(), json!("closed"));
            pool.insert("refunded".to_string(), json!(refunded));
            pool.insert("remaining".to_string(), json!(0));
            pool.insert("closeId".to_string(), json!(input.close_id));
            pool.insert("closeReason".to_string(), json!(input.reason));
            pool.insert("updatedAt".to_string(), json!(now));
            put_finance_pool(&*trx, &input.pool_id, &pool)?;
            trx.put_link(&close_marker, &input.close_id);
            let participants = vec![payer_id.clone(), authority];
            let journal_id = write_finance_journal(
                &*trx,
                "pool.closed",
                &input.pool_id,
                &payer_id,
                json!({"refunded": remaining, "withdrawableRefunded": withdrawable_refund}),
                &participants,
                now,
            )?;
            Ok(json!({"applied": true, "pool": Value::Object(pool), "journalId": journal_id}))
        },
    )
}

fn reserve_pool(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ReservePoolInput, _>(
        app,
        "/creatures/reservePool",
        finance_guard(),
        move |state: Arc<dyn IState>, input: ReservePoolInput| -> Result<Value> {
            let trx = state.trx();
            let authority_id = state.info().user_id();
            let now = Utc::now().timestamp_millis();
            if !valid_finance_id(&input.pool_id)
                || !valid_finance_id(&input.payer_user_id)
                || !valid_finance_id(&input.quote_id)
                || !valid_finance_id(&input.run_id)
            {
                return Err(anyhow!("invalid pool, payer, quote, or run identifier"));
            }
            if input.max_amount <= 0 {
                return Err(anyhow!("reservation amount must be greater than zero"));
            }
            // Idempotent by runId: a reservation record already present means the
            // run was authorized; return it rather than double-reserving.
            let reservation_key = finance_pool_reservation_key(&input.run_id);
            if let Ok(existing) = trx.get_json(&reservation_key, "reservation") {
                if existing.get("poolId").and_then(Value::as_str) == Some(input.pool_id.as_str()) {
                    return Ok(
                        json!({"applied": false, "alreadyApplied": true, "reservation": existing}),
                    );
                }
                return Err(anyhow!("run already reserved against another pool"));
            }
            let mut pool = get_finance_pool(&*trx, &input.pool_id)?;
            if pool.get("settlementAuthority").and_then(Value::as_str)
                != Some(authority_id.as_str())
            {
                return Err(anyhow!("caller is not this pool's settlement authority"));
            }
            if pool.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str())
            {
                return Err(anyhow!("payer does not match pool"));
            }
            if pool.get("status").and_then(Value::as_str) != Some("open") {
                return Err(anyhow!("pool is not open"));
            }
            let expires_at = pool.get("expiresAt").and_then(as_i64).unwrap_or(0);
            if expires_at <= 0 || now > expires_at {
                return Err(anyhow!("pool expired"));
            }
            // Bind the reservation to the client-signed, globally committed quote:
            // the meter cannot invent a payer or amount.
            let quote = trx
                .get_json(&format!("Json::BillingQuote::{}", input.quote_id), "quote")
                .map_err(|_| anyhow!("run quote not found"))?;
            if quote.get("payerUserId").and_then(Value::as_str)
                != Some(input.payer_user_id.as_str())
            {
                return Err(anyhow!("quote payer does not match reservation"));
            }
            if quote.get("requestId").and_then(Value::as_str) != Some(input.run_id.as_str()) {
                return Err(anyhow!("quote is not bound to this run"));
            }
            if quote.get("maxAmount").and_then(as_i64) != Some(input.max_amount) {
                return Err(anyhow!("reservation amount does not match the quote"));
            }
            let remaining = pool.get("remaining").and_then(as_i64).unwrap_or(0);
            if remaining < input.max_amount {
                return Err(anyhow!(
                    "pool has insufficient remaining balance for this run"
                ));
            }
            let reserved = pool
                .get("reserved")
                .and_then(as_i64)
                .unwrap_or(0)
                .checked_add(input.max_amount)
                .ok_or_else(|| anyhow!("pool reserved overflow"))?;
            pool.insert("remaining".to_string(), json!(remaining - input.max_amount));
            pool.insert("reserved".to_string(), json!(reserved));
            pool.insert("updatedAt".to_string(), json!(now));
            put_finance_pool(&*trx, &input.pool_id, &pool)?;

            let reservation = json!({
                "runId": input.run_id,
                "poolId": input.pool_id,
                "payerUserId": input.payer_user_id,
                "quoteId": input.quote_id,
                "amount": input.max_amount,
                "status": "reserved",
                "createdAt": now,
            });
            trx.put_json(&reservation_key, "reservation", &reservation, false)?;
            let participants = vec![input.payer_user_id.clone(), authority_id.clone()];
            let journal_id = write_finance_journal(
                &*trx,
                "pool.reserved",
                &input.pool_id,
                &input.payer_user_id,
                json!({"runId": input.run_id, "amount": input.max_amount}),
                &participants,
                now,
            )?;
            Ok(json!({"applied": true, "reservation": reservation, "journalId": journal_id}))
        },
    )
}

fn settle_pool(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<SettlePoolInput, _>(
        app,
        "/creatures/settlePool",
        finance_guard(),
        move |state: Arc<dyn IState>, input: SettlePoolInput| -> Result<Value> {
            let trx = state.trx();
            let authority_id = state.info().user_id();
            let now = Utc::now().timestamp_millis();
            if !valid_finance_id(&input.pool_id)
                || !valid_finance_id(&input.payer_user_id)
                || !valid_finance_id(&input.quote_id)
                || !valid_finance_id(&input.run_id)
                || !valid_finance_id(&input.settlement_id)
                || !valid_finance_hash(&input.usage_hash)
            {
                return Err(anyhow!("invalid settlement identifiers or usageHash"));
            }
            let settlement_marker = format!(
                "FinancePoolSettlement::{authority_id}::{}",
                input.settlement_id
            );
            if !trx.get_link(&settlement_marker).is_empty() {
                let pool = get_finance_pool(&*trx, &input.pool_id)?;
                return Ok(json!({"applied": false, "alreadyApplied": true, "pool": pool}));
            }
            let reservation_key = finance_pool_reservation_key(&input.run_id);
            let mut reservation = trx
                .get_json(&reservation_key, "reservation")
                .map_err(|_| anyhow!("run reservation not found"))?;
            if reservation.get("status").and_then(Value::as_str) != Some("reserved") {
                return Err(anyhow!("run reservation is not open for settlement"));
            }
            if reservation.get("poolId").and_then(Value::as_str) != Some(input.pool_id.as_str())
                || reservation.get("payerUserId").and_then(Value::as_str)
                    != Some(input.payer_user_id.as_str())
                || reservation.get("quoteId").and_then(Value::as_str)
                    != Some(input.quote_id.as_str())
            {
                return Err(anyhow!("settlement does not match the run reservation"));
            }
            let slice = reservation.get("amount").and_then(as_i64).unwrap_or(0);
            if slice <= 0 {
                return Err(anyhow!("invalid reservation amount"));
            }
            let mut pool = get_finance_pool(&*trx, &input.pool_id)?;
            if pool.get("settlementAuthority").and_then(Value::as_str)
                != Some(authority_id.as_str())
            {
                return Err(anyhow!("caller is not this pool's settlement authority"));
            }
            if pool.get("status").and_then(Value::as_str) != Some("open") {
                return Err(anyhow!("pool is not open"));
            }
            // Beneficiary caps come from the client-signed, globally committed quote
            // — the meter cannot pay an unauthorized beneficiary or exceed its caps.
            let quote = trx
                .get_json(&format!("Json::BillingQuote::{}", input.quote_id), "quote")
                .map_err(|_| anyhow!("run quote not found"))?;
            if quote.get("payerUserId").and_then(Value::as_str)
                != Some(input.payer_user_id.as_str())
            {
                return Err(anyhow!("quote payer does not match settlement"));
            }
            let beneficiaries = quote
                .get("beneficiaries")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("quote beneficiaries missing"))?;
            let mut caps: HashMap<String, i64> = HashMap::new();
            for item in beneficiaries {
                let user_id = item.get("userId").and_then(Value::as_str).unwrap_or("");
                let role = item.get("role").and_then(Value::as_str).unwrap_or("");
                let cap = item.get("maxAmount").and_then(as_i64).unwrap_or(0);
                if user_id.is_empty() || role.is_empty() || cap <= 0 {
                    return Err(anyhow!("invalid quote beneficiary"));
                }
                caps.insert(format!("{user_id}|{role}"), cap);
            }

            let mut actual_amount = 0_i64;
            let mut allocated: HashMap<String, i64> = HashMap::new();
            let mut credits: HashMap<String, i64> = HashMap::new();
            for line in &input.lines {
                if line.amount <= 0
                    || !valid_finance_id(&line.user_id)
                    || !valid_finance_id(&line.role)
                    || line.source_ref.len() > 256
                {
                    return Err(anyhow!("invalid settlement line"));
                }
                if line.user_id == input.payer_user_id {
                    return Err(anyhow!("payer cannot be a settlement beneficiary"));
                }
                let cap_key = format!("{}|{}", line.user_id, line.role);
                let Some(cap) = caps.get(&cap_key) else {
                    return Err(anyhow!(
                        "settlement beneficiary role not authorized by quote"
                    ));
                };
                actual_amount = actual_amount
                    .checked_add(line.amount)
                    .ok_or_else(|| anyhow!("settlement amount overflow"))?;
                let role_total = allocated.entry(cap_key).or_insert(0);
                *role_total = role_total
                    .checked_add(line.amount)
                    .ok_or_else(|| anyhow!("beneficiary role amount overflow"))?;
                if *role_total > *cap {
                    return Err(anyhow!("settlement exceeds beneficiary role cap"));
                }
                let credited = credits.entry(line.user_id.clone()).or_insert(0);
                *credited = credited
                    .checked_add(line.amount)
                    .ok_or_else(|| anyhow!("beneficiary amount overflow"))?;
            }
            if actual_amount > slice {
                return Err(anyhow!("settlement exceeds the run reservation"));
            }

            let mut participants = vec![input.payer_user_id.clone(), authority_id.clone()];
            for (user_id, amount) in &credits {
                add_finance_counter(&*trx, &format!("FinanceEarned::{user_id}"), *amount)?;
                let Some(mut receiver) =
                    (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                        .account(&user_id.clone())?
                else {
                    return Err(anyhow!("settlement beneficiary not found"));
                };
                let debt = finance_debt_amount(&*trx, user_id)?;
                let debt_repaid = debt.min(*amount);
                let wallet_credit = amount
                    .checked_sub(debt_repaid)
                    .ok_or_else(|| anyhow!("beneficiary credit underflow"))?;
                receiver.balance = receiver
                    .balance
                    .checked_add(wallet_credit)
                    .ok_or_else(|| anyhow!("beneficiary balance overflow"))?;
                let withdrawable = finance_withdrawable_amount(&*trx, user_id)?
                    .checked_add(wallet_credit)
                    .ok_or_else(|| anyhow!("withdrawable earnings overflow"))?;
                set_finance_debt_amount(&*trx, user_id, debt - debt_repaid)?;
                set_finance_withdrawable_amount(&*trx, user_id, withdrawable)?;
                (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .store_account(&receiver)?;
                participants.push(user_id.clone());
            }

            // The spent portion leaves the payer's held funds to beneficiaries; the
            // unused slice (slice - actual) returns to the pool's remaining and stays
            // held. held decreases by actual only.
            let refund_to_pool = slice
                .checked_sub(actual_amount)
                .ok_or_else(|| anyhow!("reservation refund underflow"))?;
            if actual_amount > 0 {
                let held = finance_held_amount(&*trx, &input.payer_user_id)?
                    .checked_sub(actual_amount)
                    .ok_or_else(|| anyhow!("held balance underflow"))?;
                set_finance_held_amount(&*trx, &input.payer_user_id, held)?;
                add_finance_counter(
                    &*trx,
                    &format!("FinanceSpent::{}", input.payer_user_id),
                    actual_amount,
                )?;
            }

            let remaining = pool
                .get("remaining")
                .and_then(as_i64)
                .unwrap_or(0)
                .checked_add(refund_to_pool)
                .ok_or_else(|| anyhow!("pool remaining overflow"))?;
            let reserved = pool
                .get("reserved")
                .and_then(as_i64)
                .unwrap_or(0)
                .checked_sub(slice)
                .ok_or_else(|| anyhow!("pool reserved underflow"))?;
            let spent = pool
                .get("spent")
                .and_then(as_i64)
                .unwrap_or(0)
                .checked_add(actual_amount)
                .ok_or_else(|| anyhow!("pool spent overflow"))?;
            pool.insert("remaining".to_string(), json!(remaining));
            pool.insert("reserved".to_string(), json!(reserved));
            pool.insert("spent".to_string(), json!(spent));
            pool.insert("updatedAt".to_string(), json!(now));
            put_finance_pool(&*trx, &input.pool_id, &pool)?;

            reservation.insert("status".to_string(), json!("settled"));
            reservation.insert("settlementId".to_string(), json!(input.settlement_id));
            reservation.insert("actualAmount".to_string(), json!(actual_amount));
            reservation.insert("usageHash".to_string(), json!(input.usage_hash));
            // Persist the lines so reconciliation can replay pool earnings/spend the
            // same way it replays settled holds' settlementLines.
            reservation.insert(
                "settlementLines".to_string(),
                serde_json::to_value(&input.lines).unwrap_or(Value::Null),
            );
            reservation.insert("settledAt".to_string(), json!(now));
            trx.put_json(
                &reservation_key,
                "reservation",
                &Value::Object(reservation),
                false,
            )?;
            trx.put_link(&settlement_marker, &input.run_id);

            let journal_id = write_finance_journal(
                &*trx,
                "pool.settled",
                &input.pool_id,
                &input.payer_user_id,
                json!({"runId": input.run_id, "actual": actual_amount, "refundedToPool": refund_to_pool}),
                &participants,
                now,
            )?;
            Ok(json!({
                "applied": true,
                "actualAmount": actual_amount,
                "refundedToPool": refund_to_pool,
                "journalId": journal_id,
            }))
        },
    )
}

fn release_pool(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ReleasePoolInput, _>(
        app,
        "/creatures/releasePool",
        finance_guard(),
        move |state: Arc<dyn IState>, input: ReleasePoolInput| -> Result<Value> {
            let trx = state.trx();
            let caller_id = state.info().user_id();
            let now = Utc::now().timestamp_millis();
            if !valid_finance_id(&input.pool_id)
                || !valid_finance_id(&input.payer_user_id)
                || !valid_finance_id(&input.run_id)
                || !valid_finance_id(&input.release_id)
            {
                return Err(anyhow!("invalid pool, payer, run, or release identifier"));
            }
            let reservation_key = finance_pool_reservation_key(&input.run_id);
            let mut reservation = trx
                .get_json(&reservation_key, "reservation")
                .map_err(|_| anyhow!("run reservation not found"))?;
            let status = reservation
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("");
            if status == "released"
                && reservation.get("releaseId").and_then(Value::as_str)
                    == Some(input.release_id.as_str())
            {
                return Ok(
                    json!({"applied": false, "alreadyApplied": true, "reservation": reservation}),
                );
            }
            if status != "reserved" {
                return Err(anyhow!("run reservation is not open for release"));
            }
            if reservation.get("poolId").and_then(Value::as_str) != Some(input.pool_id.as_str())
                || reservation.get("payerUserId").and_then(Value::as_str)
                    != Some(input.payer_user_id.as_str())
            {
                return Err(anyhow!("release does not match the run reservation"));
            }
            let slice = reservation.get("amount").and_then(as_i64).unwrap_or(0);
            let mut pool = get_finance_pool(&*trx, &input.pool_id)?;
            let authority = pool
                .get("settlementAuthority")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let expires_at = pool.get("expiresAt").and_then(as_i64).unwrap_or(0);
            // The settlement authority may release a run's reservation anytime; the
            // payer may recover it once the pool has expired.
            let payer_recovery =
                caller_id == input.payer_user_id && expires_at > 0 && now > expires_at;
            if caller_id != authority && !payer_recovery {
                return Err(anyhow!("caller may not release this reservation"));
            }
            // Return the whole slice to the pool; funds stay held as pool.remaining,
            // so no wallet movement (held == remaining + reserved is preserved).
            let remaining = pool
                .get("remaining")
                .and_then(as_i64)
                .unwrap_or(0)
                .checked_add(slice)
                .ok_or_else(|| anyhow!("pool remaining overflow"))?;
            let reserved = pool
                .get("reserved")
                .and_then(as_i64)
                .unwrap_or(0)
                .checked_sub(slice)
                .ok_or_else(|| anyhow!("pool reserved underflow"))?;
            pool.insert("remaining".to_string(), json!(remaining));
            pool.insert("reserved".to_string(), json!(reserved));
            pool.insert("updatedAt".to_string(), json!(now));
            put_finance_pool(&*trx, &input.pool_id, &pool)?;

            reservation.insert("status".to_string(), json!("released"));
            reservation.insert("releaseId".to_string(), json!(input.release_id));
            reservation.insert("releaseReason".to_string(), json!(input.reason));
            reservation.insert("releasedAt".to_string(), json!(now));
            trx.put_json(
                &reservation_key,
                "reservation",
                &Value::Object(reservation),
                false,
            )?;

            let participants = vec![input.payer_user_id.clone(), authority];
            let journal_id = write_finance_journal(
                &*trx,
                "pool.released",
                &input.pool_id,
                &input.payer_user_id,
                json!({"runId": input.run_id, "amount": slice}),
                &participants,
                now,
            )?;
            Ok(json!({"applied": true, "amount": slice, "journalId": journal_id}))
        },
    )
}

// Live incremental debit against the shared pool. There is NO per-run
// reservation or ceiling: a run charges actual accrued cost straight out of the
// pool's shared `remaining` as it works. All of a payer's concurrent runs across
// every space draw down the same `remaining`, so this is the single live counter
// that decides when the wallet is empty. When a debit would exceed `remaining`,
// nothing is charged and the op returns `{applied:false, exhausted:true}` — the
// meter reads that as "pool empty" and stops the run peacefully. Idempotent by
// (authority, debitId): each checkpoint carries a unique debitId, so a retried
// checkpoint never double-charges. Beneficiary crediting mirrors settlePool
// (debt repaid first, then wallet + withdrawable); the quote authorizes WHICH
// beneficiaries may be paid, while the pool's `remaining` is the only amount
// ceiling.
fn debit_pool(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<DebitPoolInput, _>(
        app,
        "/creatures/debitPool",
        finance_guard(),
        move |state: Arc<dyn IState>, input: DebitPoolInput| -> Result<Value> {
            let trx = state.trx();
            let authority_id = state.info().user_id();
            let now = Utc::now().timestamp_millis();
            if !valid_finance_id(&input.pool_id)
                || !valid_finance_id(&input.payer_user_id)
                || !valid_finance_id(&input.quote_id)
                || !valid_finance_id(&input.run_id)
                || !valid_finance_id(&input.debit_id)
                || !valid_finance_hash(&input.usage_hash)
            {
                return Err(anyhow!("invalid debit identifiers or usageHash"));
            }
            // Idempotent by debitId: a checkpoint already applied returns its prior
            // effect without charging again.
            let debit_marker = format!("FinancePoolDebit::{authority_id}::{}", input.debit_id);
            if !trx.get_link(&debit_marker).is_empty() {
                let pool = get_finance_pool(&*trx, &input.pool_id)?;
                let remaining = pool.get("remaining").and_then(as_i64).unwrap_or(0);
                return Ok(
                    json!({"applied": false, "alreadyApplied": true, "remaining": remaining}),
                );
            }
            let mut pool = get_finance_pool(&*trx, &input.pool_id)?;
            if pool.get("settlementAuthority").and_then(Value::as_str)
                != Some(authority_id.as_str())
            {
                return Err(anyhow!("caller is not this pool's settlement authority"));
            }
            if pool.get("payerUserId").and_then(Value::as_str) != Some(input.payer_user_id.as_str())
            {
                return Err(anyhow!("payer does not match pool"));
            }
            if pool.get("status").and_then(Value::as_str) != Some("open") {
                return Err(anyhow!("pool is not open"));
            }
            let expires_at = pool.get("expiresAt").and_then(as_i64).unwrap_or(0);
            if expires_at <= 0 || now > expires_at {
                return Err(anyhow!("pool expired"));
            }
            // The quote (client-signed, globally committed) names the authorized
            // beneficiaries. We enforce membership of each line's (userId|role) in
            // that set, but NOT the quote's per-run amount caps — the pool's
            // remaining balance is the only amount ceiling for a live-debited run.
            let quote = trx
                .get_json(&format!("Json::BillingQuote::{}", input.quote_id), "quote")
                .map_err(|_| anyhow!("run quote not found"))?;
            if quote.get("payerUserId").and_then(Value::as_str)
                != Some(input.payer_user_id.as_str())
            {
                return Err(anyhow!("quote payer does not match debit"));
            }
            let beneficiaries = quote
                .get("beneficiaries")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("quote beneficiaries missing"))?;
            let mut authorized: HashMap<String, bool> = HashMap::new();
            for item in beneficiaries {
                let user_id = item.get("userId").and_then(Value::as_str).unwrap_or("");
                let role = item.get("role").and_then(Value::as_str).unwrap_or("");
                if user_id.is_empty() || role.is_empty() {
                    return Err(anyhow!("invalid quote beneficiary"));
                }
                authorized.insert(format!("{user_id}|{role}"), true);
            }

            let mut delta = 0_i64;
            // `credits` is keyed by (userId|role) for the reconciliation record and
            // for earned attribution; `user_credits` aggregates per user so a
            // beneficiary paid under two roles in one debit is pulled/pushed once
            // (mirrors settlePool, which never re-reads a just-written balance).
            let mut credits: HashMap<String, i64> = HashMap::new();
            let mut user_credits: HashMap<String, i64> = HashMap::new();
            for line in &input.lines {
                if line.amount <= 0
                    || !valid_finance_id(&line.user_id)
                    || !valid_finance_id(&line.role)
                    || line.source_ref.len() > 256
                {
                    return Err(anyhow!("invalid debit line"));
                }
                if line.user_id == input.payer_user_id {
                    return Err(anyhow!("payer cannot be a debit beneficiary"));
                }
                let cap_key = format!("{}|{}", line.user_id, line.role);
                if !authorized.contains_key(&cap_key) {
                    return Err(anyhow!("debit beneficiary role not authorized by quote"));
                }
                delta = delta
                    .checked_add(line.amount)
                    .ok_or_else(|| anyhow!("debit amount overflow"))?;
                let credited = credits.entry(cap_key).or_insert(0);
                *credited = credited
                    .checked_add(line.amount)
                    .ok_or_else(|| anyhow!("beneficiary amount overflow"))?;
                let user_credited = user_credits.entry(line.user_id.clone()).or_insert(0);
                *user_credited = user_credited
                    .checked_add(line.amount)
                    .ok_or_else(|| anyhow!("beneficiary amount overflow"))?;
            }
            if delta <= 0 {
                return Err(anyhow!("debit must charge a positive amount"));
            }

            let remaining = pool.get("remaining").and_then(as_i64).unwrap_or(0);
            // Peaceful exhaustion: the pool cannot cover this delta. Charge nothing
            // and tell the meter to stop the run. `remaining >= delta` below also
            // guarantees the payer's held balance (== remaining + reserved) covers
            // the debit, so the held subtraction cannot underflow.
            if remaining < delta {
                return Ok(json!({
                    "applied": false,
                    "exhausted": true,
                    "remaining": remaining,
                    "charged": 0,
                }));
            }

            let mut participants = vec![input.payer_user_id.clone(), authority_id.clone()];
            // Credit each authorized beneficiary once, aggregated across roles/lines.
            for (user_id, amount) in &user_credits {
                let user_id = user_id.as_str();
                if user_id.is_empty() {
                    return Err(anyhow!("invalid credit beneficiary"));
                }
                add_finance_counter(&*trx, &format!("FinanceEarned::{user_id}"), *amount)?;
                let Some(mut receiver) =
                    (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                        .account(&user_id.to_string())?
                else {
                    return Err(anyhow!("debit beneficiary not found"));
                };
                let debt = finance_debt_amount(&*trx, user_id)?;
                let debt_repaid = debt.min(*amount);
                let wallet_credit = amount
                    .checked_sub(debt_repaid)
                    .ok_or_else(|| anyhow!("beneficiary credit underflow"))?;
                receiver.balance = receiver
                    .balance
                    .checked_add(wallet_credit)
                    .ok_or_else(|| anyhow!("beneficiary balance overflow"))?;
                let withdrawable = finance_withdrawable_amount(&*trx, user_id)?
                    .checked_add(wallet_credit)
                    .ok_or_else(|| anyhow!("withdrawable earnings overflow"))?;
                set_finance_debt_amount(&*trx, user_id, debt - debt_repaid)?;
                set_finance_withdrawable_amount(&*trx, user_id, withdrawable)?;
                (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .store_account(&receiver)?;
                participants.push(user_id.to_string());
            }

            // The debited amount leaves the payer's held funds for beneficiaries and
            // the pool's remaining, and is recorded as spent on both the payer and
            // the pool. held == remaining + reserved is preserved (both drop by delta).
            let held = finance_held_amount(&*trx, &input.payer_user_id)?
                .checked_sub(delta)
                .ok_or_else(|| anyhow!("held balance underflow"))?;
            set_finance_held_amount(&*trx, &input.payer_user_id, held)?;
            add_finance_counter(
                &*trx,
                &format!("FinanceSpent::{}", input.payer_user_id),
                delta,
            )?;

            let new_remaining = remaining
                .checked_sub(delta)
                .ok_or_else(|| anyhow!("pool remaining underflow"))?;
            let spent = pool
                .get("spent")
                .and_then(as_i64)
                .unwrap_or(0)
                .checked_add(delta)
                .ok_or_else(|| anyhow!("pool spent overflow"))?;
            pool.insert("remaining".to_string(), json!(new_remaining));
            pool.insert("spent".to_string(), json!(spent));
            pool.insert("updatedAt".to_string(), json!(now));
            put_finance_pool(&*trx, &input.pool_id, &pool)?;

            // Per-run accumulator: fold this checkpoint's charge and credits into the
            // run's running total so reconciliation replays live-debited spend and
            // earnings the same way it replays a settled reservation.
            let debit_key = finance_live_debit_key(&input.run_id);
            let mut record = trx
                .get_json(&debit_key, "debit")
                .unwrap_or_else(|_| Map::new());
            if record.is_empty() {
                record.insert("runId".to_string(), json!(input.run_id));
                record.insert("poolId".to_string(), json!(input.pool_id));
                record.insert("payerUserId".to_string(), json!(input.payer_user_id));
                record.insert("quoteId".to_string(), json!(input.quote_id));
                record.insert("createdAt".to_string(), json!(now));
            }
            let charged_total = record
                .get("chargedTotal")
                .and_then(as_i64)
                .unwrap_or(0)
                .checked_add(delta)
                .ok_or_else(|| anyhow!("run charged total overflow"))?;
            record.insert("chargedTotal".to_string(), json!(charged_total));
            let mut record_credits = record
                .get("credits")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_else(Map::new);
            for (cap_key, amount) in &credits {
                let prior = record_credits
                    .get(cap_key)
                    .and_then(as_i64)
                    .unwrap_or(0)
                    .checked_add(*amount)
                    .ok_or_else(|| anyhow!("run credit overflow"))?;
                record_credits.insert(cap_key.clone(), json!(prior));
            }
            record.insert("credits".to_string(), Value::Object(record_credits));
            record.insert("lastDebitId".to_string(), json!(input.debit_id));
            record.insert("lastUsageHash".to_string(), json!(input.usage_hash));
            record.insert("updatedAt".to_string(), json!(now));
            trx.put_json(&debit_key, "debit", &Value::Object(record), false)?;
            trx.put_link(&debit_marker, &input.run_id);

            let journal_id = write_finance_journal(
                &*trx,
                "pool.debited",
                &input.pool_id,
                &input.payer_user_id,
                json!({"runId": input.run_id, "amount": delta, "remaining": new_remaining}),
                &participants,
                now,
            )?;
            Ok(json!({
                "applied": true,
                "charged": delta,
                "remaining": new_remaining,
                "journalId": journal_id,
            }))
        },
    )
}

fn reconcile_financial_system(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ReconcileFinancialSystemInput, _>(
        app,
        "/creatures/reconcileFinancialSystem",
        finance_guard(),
        move |state: Arc<dyn IState>, input: ReconcileFinancialSystemInput| -> Result<Value> {
            if state.info().user_id() != "1@global" {
                return Err(anyhow!("access denied"));
            }
            let trx = state.trx();
            let max_issues = if input.max_issues <= 0 {
                100_usize
            } else {
                input.max_issues.min(1000) as usize
            };
            let mut issues: Vec<Value> = Vec::new();
            let mut report = |code: &str, reference: &str, detail: String| {
                if issues.len() < max_issues {
                    issues.push(json!({"code": code, "reference": reference, "detail": detail}));
                }
            };
            let mut held_expected: HashMap<String, i64> = HashMap::new();
            let mut project_reserved_expected: HashMap<String, i64> = HashMap::new();
            let mut project_spent_expected: HashMap<String, i64> = HashMap::new();
            let mut spent_expected: HashMap<String, i64> = HashMap::new();
            let mut earned_expected: HashMap<String, i64> = HashMap::new();
            let mut hold_count = 0_i64;
            let mut active_hold_count = 0_i64;
            let hold_prefix = "json::Json::FinanceHold::";
            for key in trx.get_by_prefix(hold_prefix) {
                let Some(hold_id) = key
                    .strip_prefix(hold_prefix)
                    .and_then(|rest| rest.strip_suffix("::hold"))
                else {
                    continue;
                };
                let Ok(hold) = trx.get_json(&finance_hold_key(hold_id), "hold") else {
                    report(
                        "hold.unreadable",
                        hold_id,
                        "hold JSON cannot be read".to_string(),
                    );
                    continue;
                };
                hold_count += 1;
                let payer = hold
                    .get("payerUserId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let project = hold.get("projectId").and_then(Value::as_str).unwrap_or("");
                let status = hold.get("status").and_then(Value::as_str).unwrap_or("");
                let max_amount = hold.get("maxAmount").and_then(as_i64).unwrap_or(-1);
                let remaining = hold.get("remainingAmount").and_then(as_i64).unwrap_or(-1);
                if payer.is_empty() || max_amount <= 0 {
                    report(
                        "hold.invalid",
                        hold_id,
                        "payer or maxAmount is invalid".to_string(),
                    );
                    continue;
                }
                match status {
                    "open" | "running" => {
                        active_hold_count += 1;
                        if remaining != max_amount {
                            report(
                                "hold.remaining_mismatch",
                                hold_id,
                                format!("remaining={remaining}, max={max_amount}"),
                            );
                        }
                        if !finance_map_add(&mut held_expected, payer, max_amount) {
                            report(
                                "held.overflow",
                                payer,
                                "expected held balance overflow".to_string(),
                            );
                        }
                        if !project.is_empty()
                            && !finance_map_add(&mut project_reserved_expected, project, max_amount)
                        {
                            report(
                                "project.reserved_overflow",
                                project,
                                "expected reservation overflow".to_string(),
                            );
                        }
                    }
                    "settled" => {
                        let actual = hold.get("actualAmount").and_then(as_i64).unwrap_or(-1);
                        let refunded = hold.get("refundedAmount").and_then(as_i64).unwrap_or(-1);
                        if remaining != 0
                            || actual < 0
                            || refunded < 0
                            || actual.checked_add(refunded) != Some(max_amount)
                        {
                            report("hold.settlement_mismatch", hold_id, format!("actual={actual}, refunded={refunded}, max={max_amount}, remaining={remaining}"));
                            continue;
                        }
                        let mut line_total = 0_i64;
                        for line in hold
                            .get("settlementLines")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                        {
                            let user_id = line.get("userId").and_then(Value::as_str).unwrap_or("");
                            let amount = line.get("amount").and_then(as_i64).unwrap_or(-1);
                            if amount <= 0
                                || !finance_map_add(&mut earned_expected, user_id, amount)
                            {
                                report(
                                    "settlement.line_invalid",
                                    hold_id,
                                    "invalid beneficiary settlement line".to_string(),
                                );
                                continue;
                            }
                            line_total = line_total.checked_add(amount).unwrap_or(i64::MAX);
                        }
                        if line_total != actual {
                            report(
                                "settlement.lines_mismatch",
                                hold_id,
                                format!("lines={line_total}, actual={actual}"),
                            );
                        }
                        if !finance_map_add(&mut spent_expected, payer, actual) {
                            report(
                                "spent.overflow",
                                payer,
                                "expected spent counter overflow".to_string(),
                            );
                        }
                        if !project.is_empty()
                            && !finance_map_add(&mut project_spent_expected, project, actual)
                        {
                            report(
                                "project.spent_overflow",
                                project,
                                "expected project spend overflow".to_string(),
                            );
                        }
                    }
                    "released" | "expired" => {
                        let refunded = hold.get("refundedAmount").and_then(as_i64).unwrap_or(-1);
                        if remaining != 0 || refunded != max_amount {
                            report(
                                "hold.release_mismatch",
                                hold_id,
                                format!(
                                    "refunded={refunded}, max={max_amount}, remaining={remaining}"
                                ),
                            );
                        }
                    }
                    _ => report("hold.status_invalid", hold_id, format!("status={status}")),
                }
            }

            // Pools hold wallet funds (remaining + reserved while open) and must
            // satisfy maxAmount == remaining + reserved + spent + refunded.
            let pool_prefix = "json::Json::FinancePool::";
            for key in trx.get_by_prefix(pool_prefix) {
                let Some(pool_id) = key
                    .strip_prefix(pool_prefix)
                    .and_then(|rest| rest.strip_suffix("::pool"))
                else {
                    continue;
                };
                let Ok(pool) = trx.get_json(&finance_pool_key(pool_id), "pool") else {
                    report(
                        "pool.unreadable",
                        pool_id,
                        "pool JSON cannot be read".to_string(),
                    );
                    continue;
                };
                let payer = pool
                    .get("payerUserId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let status = pool.get("status").and_then(Value::as_str).unwrap_or("");
                let max_amount = pool.get("maxAmount").and_then(as_i64).unwrap_or(-1);
                let remaining = pool.get("remaining").and_then(as_i64).unwrap_or(-1);
                let reserved = pool.get("reserved").and_then(as_i64).unwrap_or(-1);
                let spent = pool.get("spent").and_then(as_i64).unwrap_or(-1);
                let refunded = pool.get("refunded").and_then(as_i64).unwrap_or(-1);
                if payer.is_empty()
                    || max_amount < 0
                    || remaining < 0
                    || reserved < 0
                    || spent < 0
                    || refunded < 0
                {
                    report(
                        "pool.invalid",
                        pool_id,
                        "payer or pool amounts are invalid".to_string(),
                    );
                    continue;
                }
                let sum = remaining
                    .checked_add(reserved)
                    .and_then(|v| v.checked_add(spent))
                    .and_then(|v| v.checked_add(refunded));
                if sum != Some(max_amount) {
                    report("pool.balance_mismatch", pool_id, format!("remaining={remaining}, reserved={reserved}, spent={spent}, refunded={refunded}, max={max_amount}"));
                }
                if status == "open"
                    && !finance_map_add(
                        &mut held_expected,
                        payer,
                        remaining.saturating_add(reserved),
                    )
                {
                    report(
                        "held.overflow",
                        payer,
                        "expected held (pool) overflow".to_string(),
                    );
                }
            }

            // Pool reservations: an open one still holds its slice in the pool's
            // `reserved`; a settled one contributes to spent/earned exactly like a
            // settled hold so those counters reconcile for pool users.
            let reservation_prefix = "json::Json::FinancePoolReservation::";
            let mut pool_reserved_expected: HashMap<String, i64> = HashMap::new();
            for key in trx.get_by_prefix(reservation_prefix) {
                let Some(run_id) = key
                    .strip_prefix(reservation_prefix)
                    .and_then(|rest| rest.strip_suffix("::reservation"))
                else {
                    continue;
                };
                let Ok(reservation) =
                    trx.get_json(&finance_pool_reservation_key(run_id), "reservation")
                else {
                    report(
                        "reservation.unreadable",
                        run_id,
                        "reservation JSON cannot be read".to_string(),
                    );
                    continue;
                };
                let payer = reservation
                    .get("payerUserId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let pool_id = reservation
                    .get("poolId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let status = reservation
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let amount = reservation.get("amount").and_then(as_i64).unwrap_or(-1);
                if payer.is_empty() || pool_id.is_empty() || amount < 0 {
                    report(
                        "reservation.invalid",
                        run_id,
                        "reservation fields are invalid".to_string(),
                    );
                    continue;
                }
                match status {
                    "reserved" => {
                        if !finance_map_add(&mut pool_reserved_expected, pool_id, amount) {
                            report(
                                "reservation.overflow",
                                pool_id,
                                "expected pool reserved overflow".to_string(),
                            );
                        }
                    }
                    "settled" => {
                        let actual = reservation
                            .get("actualAmount")
                            .and_then(as_i64)
                            .unwrap_or(-1);
                        if actual < 0 {
                            report(
                                "reservation.settlement_invalid",
                                run_id,
                                "settled reservation missing actualAmount".to_string(),
                            );
                            continue;
                        }
                        let mut line_total = 0_i64;
                        for line in reservation
                            .get("settlementLines")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                        {
                            let user_id = line.get("userId").and_then(Value::as_str).unwrap_or("");
                            let line_amount = line.get("amount").and_then(as_i64).unwrap_or(-1);
                            if line_amount <= 0
                                || !finance_map_add(&mut earned_expected, user_id, line_amount)
                            {
                                report(
                                    "reservation.line_invalid",
                                    run_id,
                                    "invalid pool settlement line".to_string(),
                                );
                                continue;
                            }
                            line_total = line_total.checked_add(line_amount).unwrap_or(i64::MAX);
                        }
                        if line_total != actual {
                            report(
                                "reservation.lines_mismatch",
                                run_id,
                                format!("lines={line_total}, actual={actual}"),
                            );
                        }
                        if !finance_map_add(&mut spent_expected, payer, actual) {
                            report(
                                "spent.overflow",
                                payer,
                                "expected spent (pool) overflow".to_string(),
                            );
                        }
                    }
                    "released" => {}
                    _ => report(
                        "reservation.status_invalid",
                        run_id,
                        format!("status={status}"),
                    ),
                }
            }

            // Live pool debits: a live-debited run has no reservation, so its spend
            // and beneficiary earnings are replayed from its FinanceLiveDebit record
            // (charged straight from the pool's remaining → spent). This mirrors how a
            // settled reservation contributes to spent_expected/earned_expected, so
            // the payer FinanceSpent and beneficiary FinanceEarned counters reconcile
            // for live-metered runs too. Pool balance itself is covered by the
            // maxAmount == remaining + reserved + spent + refunded check above.
            let live_debit_prefix = "json::Json::FinanceLiveDebit::";
            for key in trx.get_by_prefix(live_debit_prefix) {
                let Some(run_id) = key
                    .strip_prefix(live_debit_prefix)
                    .and_then(|rest| rest.strip_suffix("::debit"))
                else {
                    continue;
                };
                let Ok(record) = trx.get_json(&finance_live_debit_key(run_id), "debit") else {
                    report(
                        "livedebit.unreadable",
                        run_id,
                        "live debit JSON cannot be read".to_string(),
                    );
                    continue;
                };
                let payer = record
                    .get("payerUserId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let charged = record.get("chargedTotal").and_then(as_i64).unwrap_or(-1);
                if payer.is_empty() || charged < 0 {
                    report(
                        "livedebit.invalid",
                        run_id,
                        "live debit fields are invalid".to_string(),
                    );
                    continue;
                }
                let mut credit_total = 0_i64;
                if let Some(credits) = record.get("credits").and_then(Value::as_object) {
                    for (cap_key, value) in credits {
                        let user_id = cap_key.split('|').next().unwrap_or("");
                        let amount = value.as_i64().unwrap_or(-1);
                        if user_id.is_empty()
                            || amount <= 0
                            || !finance_map_add(&mut earned_expected, user_id, amount)
                        {
                            report(
                                "livedebit.credit_invalid",
                                run_id,
                                "invalid live debit credit".to_string(),
                            );
                            continue;
                        }
                        credit_total = credit_total.checked_add(amount).unwrap_or(i64::MAX);
                    }
                }
                if credit_total != charged {
                    report(
                        "livedebit.credit_mismatch",
                        run_id,
                        format!("credits={credit_total}, charged={charged}"),
                    );
                }
                if !finance_map_add(&mut spent_expected, payer, charged) {
                    report(
                        "spent.overflow",
                        payer,
                        "expected spent (live debit) overflow".to_string(),
                    );
                }
            }

            // Each open pool's stored `reserved` must equal the sum of its open reservations.
            for key in trx.get_by_prefix(pool_prefix) {
                let Some(pool_id) = key
                    .strip_prefix(pool_prefix)
                    .and_then(|rest| rest.strip_suffix("::pool"))
                else {
                    continue;
                };
                let Ok(pool) = trx.get_json(&finance_pool_key(pool_id), "pool") else {
                    continue;
                };
                let stored = pool.get("reserved").and_then(as_i64).unwrap_or(0);
                let expected = pool_reserved_expected.get(pool_id).copied().unwrap_or(0);
                if stored != expected {
                    report(
                        "pool.reserved_mismatch",
                        pool_id,
                        format!("stored={stored}, expected={expected}"),
                    );
                }
            }

            let mut held_actual: HashMap<String, i64> = HashMap::new();
            for key in trx
                .get_links_list("FinanceHeld::", -1, -1, &[])
                .unwrap_or_default()
            {
                let payer = key.strip_prefix("FinanceHeld::").unwrap_or("");
                let raw = trx.get_link(&key);
                match raw.parse::<i64>() {
                    Ok(value) if value >= 0 => {
                        held_actual.insert(payer.to_string(), value);
                    }
                    _ => report("held.invalid", payer, format!("stored={raw}")),
                }
            }
            let mut held_users: Vec<String> = held_expected
                .keys()
                .chain(held_actual.keys())
                .cloned()
                .collect();
            held_users.sort();
            held_users.dedup();
            for payer in held_users {
                let expected = held_expected.get(&payer).copied().unwrap_or(0);
                let actual = held_actual.get(&payer).copied().unwrap_or(0);
                if actual != expected {
                    report(
                        "held.mismatch",
                        &payer,
                        format!("stored={actual}, expected={expected}"),
                    );
                }
            }

            let mut payout_held_expected: HashMap<String, i64> = HashMap::new();
            let mut payout_count = 0_i64;
            let mut pending_payout_count = 0_i64;
            let payout_prefix = "json::Json::FinancePayout::";
            for key in trx.get_by_prefix(payout_prefix) {
                let Some(payout_id) = key
                    .strip_prefix(payout_prefix)
                    .and_then(|rest| rest.strip_suffix("::payout"))
                else {
                    continue;
                };
                let Ok(payout) = get_finance_payout(&*trx, payout_id) else {
                    report(
                        "payout.unreadable",
                        payout_id,
                        "payout JSON cannot be read".to_string(),
                    );
                    continue;
                };
                payout_count += 1;
                if payout.get("status").and_then(Value::as_str) == Some("pending") {
                    pending_payout_count += 1;
                    let user_id = payout.get("userId").and_then(Value::as_str).unwrap_or("");
                    let amount = payout.get("amount").and_then(as_i64).unwrap_or(-1);
                    if amount <= 0 || !finance_map_add(&mut payout_held_expected, user_id, amount) {
                        report(
                            "payout.invalid",
                            payout_id,
                            "pending payout owner or amount is invalid".to_string(),
                        );
                    }
                }
            }
            let mut payout_held_actual: HashMap<String, i64> = HashMap::new();
            for key in trx
                .get_links_list("FinancePayoutHeld::", -1, -1, &[])
                .unwrap_or_default()
            {
                let user_id = key.strip_prefix("FinancePayoutHeld::").unwrap_or("");
                match trx.get_link(&key).parse::<i64>() {
                    Ok(value) if value >= 0 => {
                        payout_held_actual.insert(user_id.to_string(), value);
                    }
                    _ => report(
                        "payout.held_invalid",
                        user_id,
                        "stored payout held amount is invalid".to_string(),
                    ),
                }
            }
            let mut payout_users: Vec<String> = payout_held_expected
                .keys()
                .chain(payout_held_actual.keys())
                .cloned()
                .collect();
            payout_users.sort();
            payout_users.dedup();
            for user_id in payout_users {
                let expected = payout_held_expected.get(&user_id).copied().unwrap_or(0);
                let actual = payout_held_actual.get(&user_id).copied().unwrap_or(0);
                if actual != expected {
                    report(
                        "payout.held_mismatch",
                        &user_id,
                        format!("stored={actual}, expected={expected}"),
                    );
                }
            }
            let mut total_withdrawable_actual: i64 = 0;
            for key in trx
                .get_links_list("FinanceWithdrawable::", -1, -1, &[])
                .unwrap_or_default()
            {
                let user_id = key.strip_prefix("FinanceWithdrawable::").unwrap_or("");
                let withdrawable = trx.get_link(&key).parse::<i64>().unwrap_or(-1);
                // LD-13: a counter for a missing creature is now reported; the old
                // `id.is_empty()` check could never see one.
                let available =
                    crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx }
                        .account(user_id)?
                        .map(|account| account.balance);
                if withdrawable < 0 || available.is_none_or(|available| withdrawable > available) {
                    report(
                        "withdrawable.invalid",
                        user_id,
                        format!(
                            "withdrawable={withdrawable}, available={}",
                            available.unwrap_or_default()
                        ),
                    );
                }
                if withdrawable > 0 {
                    total_withdrawable_actual =
                        total_withdrawable_actual.saturating_add(withdrawable);
                }
            }
            // Withdrawable funds are created ONLY by settled earnings; spending and
            // payouts reduce them and transfers only move them between wallets, so
            // system-wide withdrawable can never exceed system-wide lifetime
            // earnings. A positive gap is unbacked withdrawable — counter drift the
            // per-wallet "withdrawable <= available" check cannot see (e.g. left
            // over from historical hold leaks, where refunds credited withdrawable
            // that was never earned). This is the system total, so it is immune to
            // transfers moving withdrawable between wallets.
            let total_earned_expected: i64 = earned_expected
                .values()
                .fold(0_i64, |acc, v| acc.saturating_add(*v));
            if total_withdrawable_actual > total_earned_expected {
                report(
                    "withdrawable.unbacked_total",
                    "",
                    format!(
                        "withdrawable_total={total_withdrawable_actual}, earned_total={total_earned_expected}"
                    ),
                );
            }

            for (prefix, expected, code) in [
                ("FinanceSpent::", &spent_expected, "spent.mismatch"),
                ("FinanceEarned::", &earned_expected, "earned.mismatch"),
            ] {
                let mut actual: HashMap<String, i64> = HashMap::new();
                for key in trx.get_links_list(prefix, -1, -1, &[]).unwrap_or_default() {
                    let user_id = key.strip_prefix(prefix).unwrap_or("");
                    if let Ok(value) = trx.get_link(&key).parse::<i64>() {
                        actual.insert(user_id.to_string(), value);
                    } else {
                        report(code, user_id, "stored counter is invalid".to_string());
                    }
                }
                let mut users: Vec<String> =
                    expected.keys().chain(actual.keys()).cloned().collect();
                users.sort();
                users.dedup();
                for user_id in users {
                    let expected_value = expected.get(&user_id).copied().unwrap_or(0);
                    let actual_value = actual.get(&user_id).copied().unwrap_or(0);
                    if actual_value != expected_value {
                        report(
                            code,
                            &user_id,
                            format!("stored={actual_value}, expected={expected_value}"),
                        );
                    }
                }
            }

            let mut projects: Vec<String> = project_reserved_expected
                .keys()
                .chain(project_spent_expected.keys())
                .cloned()
                .collect();
            let project_prefix = "json::Json::FinanceProjectBudget::";
            for key in trx.get_by_prefix(project_prefix) {
                if let Some(project) = key
                    .strip_prefix(project_prefix)
                    .and_then(|rest| rest.strip_suffix("::budget"))
                {
                    projects.push(project.to_string());
                }
            }
            projects.sort();
            projects.dedup();
            for project in projects {
                let state = trx
                    .get_json(&finance_project_budget_key(&project), "budget")
                    .unwrap_or_default();
                let stored_reserved = state.get("reservedMinor").and_then(as_i64).unwrap_or(0);
                let stored_spent = state.get("spentMinor").and_then(as_i64).unwrap_or(0);
                let expected_reserved = project_reserved_expected
                    .get(&project)
                    .copied()
                    .unwrap_or(0);
                let expected_spent = project_spent_expected.get(&project).copied().unwrap_or(0);
                if stored_reserved != expected_reserved {
                    report(
                        "project.reserved_mismatch",
                        &project,
                        format!("stored={stored_reserved}, expected={expected_reserved}"),
                    );
                }
                if stored_spent < expected_spent {
                    report(
                        "project.spent_undercount",
                        &project,
                        format!("stored={stored_spent}, minimum={expected_spent}"),
                    );
                }
            }

            let issue_count = issues.len();
            Ok(json!({
                "healthy": issue_count == 0,
                "checkedAt": Utc::now().timestamp_millis(),
                "holdCount": hold_count,
                "activeHoldCount": active_hold_count,
                "payoutCount": payout_count,
                "pendingPayoutCount": pending_payout_count,
                "payerCount": held_expected.len(),
                "projectCount": project_reserved_expected.keys().chain(project_spent_expected.keys()).collect::<std::collections::HashSet<_>>().len(),
                "issueCount": issue_count,
                "issues": issues,
            }))
        },
    )
}

fn payment_adjustment(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<PaymentAdjustmentInput, _>(
        app,
        "/creatures/paymentAdjustment",
        finance_guard(),
        move |state: Arc<dyn IState>, input: PaymentAdjustmentInput| -> Result<Value> {
            if state.info().user_id() != "1@global" {
                return Err(anyhow!("access denied"));
            }
            if !valid_finance_id(&input.user_id)
                || !valid_finance_id(&input.kind)
                || !valid_finance_id(&input.idempotency_key)
                || input.reference.is_empty()
                || input.reference.len() > 256
                || input.amount == 0
            {
                return Err(anyhow!("invalid payment adjustment"));
            }
            let allowed = matches!(
                input.kind.as_str(),
                "refund" | "chargeback" | "dispute" | "manual_debit" | "manual_credit"
            );
            if !allowed || (input.amount > 0 && input.kind != "manual_credit") {
                return Err(anyhow!("unsupported payment adjustment kind"));
            }
            if serde_json::to_vec(&input.metadata)?.len() > 4096 {
                return Err(anyhow!("payment adjustment metadata is too large"));
            }
            let trx = state.trx();
            let request_hash = finance_hash(&serde_json::to_value(&input)?)?;
            let marker = format!("PaymentAdjustment::{}", input.idempotency_key);
            let previous = trx.get_link(&marker);
            if !previous.is_empty() {
                let Some((previous_hash, journal_id)) = previous.split_once('|') else {
                    return Err(anyhow!("invalid payment adjustment idempotency record"));
                };
                if previous_hash != request_hash {
                    return Err(anyhow!(
                        "idempotency key already used with different adjustment"
                    ));
                }
                return Ok(json!({
                    "applied": false, "alreadyApplied": true, "journalId": journal_id,
                    "account": financial_account_snapshot(&*trx, &input.user_id, 20)?,
                }));
            }
            let Some(mut creature) =
                (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                    .account(&input.user_id.clone())?
            else {
                return Err(anyhow!("payment adjustment target not found"));
            };
            let old_debt = finance_debt_amount(&*trx, &input.user_id)?;
            let old_withdrawable = finance_withdrawable_amount(&*trx, &input.user_id)?;
            if old_withdrawable > creature.balance {
                return Err(anyhow!("withdrawable balance exceeds available balance"));
            }
            let (wallet_delta, debt_delta) = if input.amount < 0 {
                let reversal = input
                    .amount
                    .checked_abs()
                    .ok_or_else(|| anyhow!("payment adjustment overflow"))?;
                let available_debit = creature.balance.min(reversal);
                let debt_added = reversal
                    .checked_sub(available_debit)
                    .ok_or_else(|| anyhow!("payment adjustment underflow"))?;
                creature.balance = creature
                    .balance
                    .checked_sub(available_debit)
                    .ok_or_else(|| anyhow!("wallet adjustment underflow"))?;
                set_finance_withdrawable_amount(
                    &*trx,
                    &input.user_id,
                    old_withdrawable.min(creature.balance),
                )?;
                set_finance_debt_amount(
                    &*trx,
                    &input.user_id,
                    old_debt
                        .checked_add(debt_added)
                        .ok_or_else(|| anyhow!("wallet debt overflow"))?,
                )?;
                (-available_debit, debt_added)
            } else {
                let debt_repaid = old_debt.min(input.amount);
                let wallet_credit = input
                    .amount
                    .checked_sub(debt_repaid)
                    .ok_or_else(|| anyhow!("payment adjustment underflow"))?;
                creature.balance = creature
                    .balance
                    .checked_add(wallet_credit)
                    .ok_or_else(|| anyhow!("wallet balance overflow"))?;
                set_finance_debt_amount(&*trx, &input.user_id, old_debt - debt_repaid)?;
                (wallet_credit, -debt_repaid)
            };
            (crate::shell::api::model::creature_ports::LegacyCreatures { trx: &*trx })
                .store_account(&creature)?;
            let participants = vec![input.user_id.clone(), state.info().user_id()];
            let journal_id = write_finance_journal(
                &*trx,
                &format!("payment.{}", input.kind),
                "",
                &input.user_id,
                json!({
                    "entries": [
                        {"account": format!("wallet:{}:available", input.user_id), "amount": wallet_delta},
                        {"account": format!("wallet:{}:debt", input.user_id), "amount": debt_delta},
                        {"account": "external:payments", "amount": -input.amount}
                    ],
                    "adjustmentAmount": input.amount, "kind": input.kind,
                    "reference": input.reference, "metadata": input.metadata,
                }),
                &participants,
                Utc::now().timestamp_millis(),
            )?;
            trx.put_link(&marker, &format!("{request_hash}|{journal_id}"));
            Ok(json!({
                "applied": true, "journalId": journal_id,
                "account": financial_account_snapshot(&*trx, &input.user_id, 20)?,
            }))
        },
    )
}

pub(super) fn start_hold_handler(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    start_hold(app)
}

pub(super) fn handlers(app: Arc<dyn ICore>) -> Vec<Arc<dyn ISecureAction>> {
    vec![
        publish_finance_catalog(app.clone()),
        register_finance_node(app.clone()),
        retire_finance_node(app.clone()),
        register_finance_resource(app.clone()),
        review_finance_resource(app.clone()),
        retire_finance_resource(app.clone()),
        publish_finance_quote(app.clone()),
        create_hold(app.clone()),
        settle_hold(app.clone()),
        release_hold(app.clone()),
        open_pool(app.clone()),
        refresh_pool(app.clone()),
        close_pool(app.clone()),
        reserve_pool(app.clone()),
        settle_pool(app.clone()),
        release_pool(app.clone()),
        debit_pool(app.clone()),
        get_hold(app.clone()),
        get_financial_account(app.clone()),
        request_payout(app.clone()),
        resolve_payout(app.clone()),
        list_payouts(app.clone()),
        reconcile_financial_system(app.clone()),
        payment_adjustment(app),
    ]
}
