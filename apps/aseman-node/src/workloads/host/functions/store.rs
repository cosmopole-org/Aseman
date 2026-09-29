//! Wasm host-call entry points for Store CRUD.
//!
//! The in-process VM driver delivers wasm-originated `createStore` /
//! `deleteStore` / `getStore` / `listStores` / `updateStore` host calls to
//! these functions through `handle_unified_host_call`. We dispatch each one
//! to the canonical `Vmm::handle_store_crud` implementation (it owns the
//! transaction logic) via the global Vmm handle published by `Vmm::new`,
//! returning the resulting JSON body so the wasm program sees a real
//! response instead of an "unsupported packet" stub.

use crate::workloads::prelude::*;

fn dispatch_store(node: &Arc<Node>, op: &str, input: &JsonValue) -> String {
    node.tools().workloads().host_action_store(op, input, 0).0
}

pub(crate) fn host_fn_create_store(node: &Arc<Node>, input: &JsonValue) -> String {
    dispatch_store(node, "create", input)
}

pub(crate) fn host_fn_delete_store(node: &Arc<Node>, input: &JsonValue) -> String {
    dispatch_store(node, "delete", input)
}

pub(crate) fn host_fn_get_store(node: &Arc<Node>, input: &JsonValue) -> String {
    dispatch_store(node, "get", input)
}

pub(crate) fn host_fn_list_stores(node: &Arc<Node>, input: &JsonValue) -> String {
    dispatch_store(node, "list", input)
}

pub(crate) fn host_fn_update_store(node: &Arc<Node>, input: &JsonValue) -> String {
    dispatch_store(node, "update", input)
}
