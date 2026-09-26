//! Wasm host-call entry points for store access (member) management.
//!
//! Routes through `Vmm::handle_micro_host_action` which performs the real
//! `onaccess::<storeId>::<userId>` + `hasaccess::<userId>::<storeId>` link
//! mutations inside a hashgraph transaction.

use crate::drivers::vmm::globals::with_global_app;
use crate::drivers::vmm::prelude::*;

fn dispatch_micro(op: &str, input: &JsonValue) -> String {
    match with_global_app(|app| app.tools().workloads().host_action_micro(op, input, 0).0) {
        Some(out) => out,
        None => json!({"ok": false, "error": "vmm not initialised"}).to_string(),
    }
}

pub(crate) fn host_fn_create_access(input: &JsonValue) -> String {
    dispatch_micro("createAccess", input)
}

pub(crate) fn host_fn_delete_access(input: &JsonValue) -> String {
    dispatch_micro("deleteAccess", input)
}
