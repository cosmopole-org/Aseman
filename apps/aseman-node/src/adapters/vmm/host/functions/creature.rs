//! Wasm host-call entry points for creature CRUD. Routes through
//! `Vmm::handle_creature_crud` which performs the real persisted-state work.

use crate::drivers::vmm::globals::with_global_app;
use crate::drivers::vmm::prelude::*;

fn dispatch_creature(op: &str, input: &JsonValue) -> String {
    match with_global_app(|app| app.tools().workloads().host_action_creature(op, input, 0).0) {
        Some(out) => out,
        None => json!({"ok": false, "error": "vmm not initialised"}).to_string(),
    }
}

pub(crate) fn host_fn_create_creature(input: &JsonValue) -> String {
    dispatch_creature("create", input)
}

pub(crate) fn host_fn_delete_creature(input: &JsonValue) -> String {
    dispatch_creature("delete", input)
}

pub(crate) fn host_fn_get_creature(input: &JsonValue) -> String {
    dispatch_creature("get", input)
}

pub(crate) fn host_fn_list_creatures(input: &JsonValue) -> String {
    dispatch_creature("list", input)
}

pub(crate) fn host_fn_update_creature(input: &JsonValue) -> String {
    dispatch_creature("update", input)
}
