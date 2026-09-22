//! Wasm host-call entry points for Store CRUD.
//!
//! The in-process VM driver delivers wasm-originated `createStore` /
//! `deleteStore` / `getStore` / `listStores` / `updateStore` host calls to
//! these functions through `handle_unified_host_call`. We dispatch each one
//! to the canonical `Vmm::handle_store_crud` implementation (it owns the
//! transaction logic) via the global Vmm handle published by `Vmm::new`,
//! returning the resulting JSON body so the wasm program sees a real
//! response instead of an "unsupported packet" stub.

use crate::drivers::vmm::globals::with_global_app;
use crate::drivers::vmm::prelude::*;

fn dispatch_store(op: &str, input: &JsonValue) -> String {
    match with_global_app(|app| app.tools().workloads().host_action_store(op, input, 0).0) {
        Some(out) => out,
        None => json!({"ok": false, "error": "vmm not initialised"}).to_string(),
    }
}

pub(crate) fn host_fn_create_store(input: &JsonValue) -> String {
    dispatch_store("create", input)
}

pub(crate) fn host_fn_delete_store(input: &JsonValue) -> String {
    dispatch_store("delete", input)
}

pub(crate) fn host_fn_get_store(input: &JsonValue) -> String {
    dispatch_store("get", input)
}

pub(crate) fn host_fn_list_stores(input: &JsonValue) -> String {
    dispatch_store("list", input)
}

pub(crate) fn host_fn_update_store(input: &JsonValue) -> String {
    dispatch_store("update", input)
}
