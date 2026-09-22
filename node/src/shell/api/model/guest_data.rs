//! Guest data routing (ADR 0021, ADR 0028, A405, ADR 0026).
//!
//! On the legacy provider, guest data stays in the node store and the existing code
//! serves it. On PostgreSQL it is served from each creature's own guest database:
//! the creature's active binding (`core.guest_database_binding`), through the guest
//! proxy (`PostgresGuestKv`). A creature without an active binding is refused rather
//! than served from legacy, which would split its data across two stores.

use std::sync::OnceLock;

use aseman_capsule_repositories::workload::CapsuleWorkloads;
use aseman_contracts::legacy_realtime::deterministic_legacy_capsule_id;
use aseman_domain::guest::{GuestKvOperation, GuestKvOutcome, LegacyKvNamespace, MAX_GUEST_LIST};
use aseman_domain::{BindingStatus, CreatureId, Uuid};
use aseman_ports::{CreatureDatabaseBindings, GuestKv};
use aseman_storage_postgres::guest::PostgresGuestKv;
use aseman_storage_postgres::PostgresCapsuleRepository;
use serde_json::{json, Value};

struct GuestRouting {
    kv: PostgresGuestKv,
    catalog: PostgresCapsuleRepository,
}

static ROUTING: OnceLock<GuestRouting> = OnceLock::new();

/// Serve guest data from creature databases (called once when the node runs on
/// PostgreSQL).
pub(crate) fn install_postgres(
    kv: PostgresGuestKv,
    catalog: PostgresCapsuleRepository,
) -> anyhow::Result<()> {
    ROUTING
        .set(GuestRouting { kv, catalog })
        .map_err(|_| anyhow::anyhow!("guest data routing is already installed"))
}

fn execute(
    routing: &GuestRouting,
    creature: &str,
    operation: &GuestKvOperation,
) -> Result<GuestKvOutcome, String> {
    if creature.trim().is_empty() {
        return Err("guest data needs an identified creature".to_owned());
    }
    let creature_id = CreatureId::from_uuid(Uuid::from_bytes(deterministic_legacy_capsule_id(
        "Creature",
        creature.as_bytes(),
    )));
    let binding = CapsuleWorkloads {
        repository: &routing.catalog,
    }
    .binding_for(creature_id)
    .map_err(|error| error.to_string())?
    .filter(|binding| binding.status == BindingStatus::Active)
    .ok_or_else(|| "the creature's guest database is not active".to_owned())?;
    routing
        .kv
        .execute(&binding, operation)
        .map_err(|error| error.to_string())
}

/// Whether guest data is served from creature databases.
pub(crate) fn on_postgres() -> bool {
    ROUTING.get().is_some()
}

/// The confined document and link calls (ADR 0028), in their legacy response shapes.
pub(crate) fn route_state(
    creature: &str,
    op: &str,
    input: &Value,
) -> Option<Result<Value, String>> {
    Some(state_with(ROUTING.get()?, creature, op, input))
}

fn state_with(
    routing: &GuestRouting,
    creature: &str,
    op: &str,
    input: &Value,
) -> Result<Value, String> {
    let key = input["key"].as_str().unwrap_or("").to_owned();
    let path = input["path"].as_str().unwrap_or("").to_owned();
    let needs_key = matches!(op, "putJson" | "getJson" | "delKey" | "getLink");
    if needs_key && key.is_empty() {
        return Err("key is required".to_owned());
    }
    let operation = match op {
        "putJson" => GuestKvOperation::PutJson {
            key,
            path,
            data: input["data"].to_string(),
            merge: input["merge"].as_bool().unwrap_or(true),
        },
        "getJson" => GuestKvOperation::GetJson { key, path },
        "getByPrefix" => GuestKvOperation::ListJson {
            prefix: input["prefix"].as_str().unwrap_or("").to_owned(),
            limit: MAX_GUEST_LIST,
        },
        "delKey" => GuestKvOperation::DeleteJson { key, path },
        "getLink" => GuestKvOperation::Get {
            namespace: LegacyKvNamespace::DbOp,
            key,
        },
        other => return Err(format!("unsupported guest state op: {other}")),
    };
    execute(routing, creature, &operation).map(|outcome| match outcome {
        GuestKvOutcome::Document { data } => json!({
            "ok": true,
            "data": serde_json::from_str::<Value>(&data).unwrap_or_else(|_| json!({})),
        }),
        GuestKvOutcome::Keys { keys } => json!({"ok": true, "data": keys}),
        GuestKvOutcome::Value { value } => json!({"ok": true, "value": value.unwrap_or_default()}),
        _ => json!({"ok": true}),
    })
}

/// A key/value `dbOp` (`put`/`get`/`del`/`getByPrefix`) in `namespace`, in the legacy
/// `vm_db_op` response shapes. `getByPrefix` returns the values of committed pairs,
/// which legacy never did (ADR 0021).
pub(crate) fn route_db_op(
    creature: &str,
    namespace: LegacyKvNamespace,
    op: &str,
    key: &str,
    value: &str,
    prefix: &str,
) -> Option<Result<String, String>> {
    Some(db_op_with(
        ROUTING.get()?,
        creature,
        namespace,
        op,
        key,
        value,
        prefix,
    ))
}

fn db_op_with(
    routing: &GuestRouting,
    creature: &str,
    namespace: LegacyKvNamespace,
    op: &str,
    key: &str,
    value: &str,
    prefix: &str,
) -> Result<String, String> {
    let operation = match op {
        "put" => GuestKvOperation::Put {
            namespace,
            key: key.to_owned(),
            value: value.to_owned(),
        },
        "get" => GuestKvOperation::Get {
            namespace,
            key: key.to_owned(),
        },
        "del" => GuestKvOperation::Delete {
            namespace,
            key: key.to_owned(),
        },
        "getByPrefix" => GuestKvOperation::List {
            namespace,
            prefix: prefix.to_owned(),
            limit: MAX_GUEST_LIST,
        },
        other => return Err(format!("unsupported dbOp: {other}")),
    };
    execute(routing, creature, &operation).map(|outcome| match outcome {
        GuestKvOutcome::Value { value } => json!({"data": value.unwrap_or_default()}).to_string(),
        GuestKvOutcome::Listed { pairs } => json!({
            "data": pairs.into_iter().map(|(_, value)| value).collect::<Vec<_>>()
        })
        .to_string(),
        _ => "{}".to_owned(),
    })
}

/// Split a runtime `dbOp` key `{creature}::{guestKey}`; runtimes build it from the
/// node-assigned machine id, so the first segment is the creature.
pub(crate) fn split_runtime_key(key: &str) -> Option<(&str, &str)> {
    key.split_once("::")
        .filter(|(creature, _)| creature.contains('@'))
}

#[cfg(test)]
mod tests;
