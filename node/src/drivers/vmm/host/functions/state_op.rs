//! `stateOp`: a remote runtime's creature-scoped key/value state (the SDK runtime
//! key space `{creature}::{key}`, ADR 0021 `dbop` namespace), served for a VMM
//! backend acting as its workload (P5-04). The key is always the caller's own: the
//! creature comes from the resolved caller, never from the input.

use crate::drivers::vmm::host::vm_host_functions::HostHierarchy;
use crate::drivers::vmm::prelude::*;

pub(crate) fn host_fn_state_op(ctx: &HostHierarchy, input: &JsonValue) -> String {
    let op = input["op"].as_str().unwrap_or("");
    if !matches!(op, "get" | "put" | "del" | "getByPrefix") {
        return json!({"ok": false, "error": "unsupported state op"}).to_string();
    }
    if ctx.creature_id.is_empty() {
        return json!({"ok": false, "error": "this operation needs an identified caller"})
            .to_string();
    }
    match crate::shell::api::model::guest_data::route_db_op(
        &ctx.creature_id,
        aseman_domain::guest::LegacyKvNamespace::DbOp,
        op,
        input["key"].as_str().unwrap_or(""),
        input["val"].as_str().unwrap_or(""),
        input["prefix"].as_str().unwrap_or(""),
    ) {
        Some(Ok(answer)) => answer,
        Some(Err(error)) => json!({"ok": false, "error": error}).to_string(),
        // Remote runtimes run only when guest data lives in creature databases.
        None => {
            json!({"ok": false, "error": "stateOp needs the PostgreSQL guest store"}).to_string()
        }
    }
}
