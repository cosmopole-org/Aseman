//! `stateOp`: a remote runtime's creature-scoped key/value state (the SDK runtime
//! key space `{creature}::{key}`, ADR 0021 `dbop` namespace), served for a VMM
//! backend acting as its workload. The key is always the caller's own: the
//! creature comes from the resolved caller, never from the input.

use crate::workloads::host::vm_host_functions::HostHierarchy;
use crate::workloads::prelude::*;

pub(crate) fn host_fn_state_op(node: &Arc<Node>, ctx: &HostHierarchy, input: &JsonValue) -> String {
    let op = input["op"].as_str().unwrap_or("");
    if !matches!(op, "get" | "put" | "del" | "getByPrefix") {
        return json!({"ok": false, "error": "unsupported state op"}).to_string();
    }
    if ctx.creature_id.is_empty() {
        return json!({"ok": false, "error": "this operation needs an identified caller"})
            .to_string();
    }
    let Some(routing) = node.guest_data() else {
        return json!({"ok": false, "error": "guest data is not available yet"}).to_string();
    };
    match crate::state::guest_data::route_db_op(
        routing,
        &ctx.creature_id,
        aseman_domain::guest::LegacyKvNamespace::DbOp,
        op,
        input["key"].as_str().unwrap_or(""),
        input["val"].as_str().unwrap_or(""),
        input["prefix"].as_str().unwrap_or(""),
    ) {
        Ok(answer) => answer,
        Err(error) => json!({"ok": false, "error": error}).to_string(),
    }
}
