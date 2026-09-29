//! Wasm host-call entry points for creature CRUD. Routes through
//! `Vmm::handle_creature_crud` which performs the real persisted-state work.

use crate::workloads::prelude::*;

fn dispatch_creature(node: &Arc<Node>, op: &str, input: &JsonValue) -> String {
    node.tools()
        .workloads()
        .host_action_creature(op, input, 0)
        .0
}

pub(crate) fn host_fn_create_creature(node: &Arc<Node>, input: &JsonValue) -> String {
    dispatch_creature(node, "create", input)
}

pub(crate) fn host_fn_delete_creature(node: &Arc<Node>, input: &JsonValue) -> String {
    dispatch_creature(node, "delete", input)
}

pub(crate) fn host_fn_get_creature(node: &Arc<Node>, input: &JsonValue) -> String {
    dispatch_creature(node, "get", input)
}

pub(crate) fn host_fn_list_creatures(node: &Arc<Node>, input: &JsonValue) -> String {
    dispatch_creature(node, "list", input)
}

pub(crate) fn host_fn_update_creature(node: &Arc<Node>, input: &JsonValue) -> String {
    dispatch_creature(node, "update", input)
}
