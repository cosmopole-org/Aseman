//! Wasm host-call entry points for store access (member) management.
//!
//! Routes through `Vmm::handle_micro_host_action` which performs the real
//! `onaccess::<storeId>::<userId>` + `hasaccess::<userId>::<storeId>` link
//! mutations inside a hashgraph transaction.

use crate::workloads::prelude::*;

fn dispatch_micro(node: &Arc<Node>, op: &str, input: &JsonValue) -> String {
    node.tools().workloads().host_action_micro(op, input, 0).0
}

pub(crate) fn host_fn_create_access(node: &Arc<Node>, input: &JsonValue) -> String {
    dispatch_micro(node, "createAccess", input)
}

pub(crate) fn host_fn_delete_access(node: &Arc<Node>, input: &JsonValue) -> String {
    dispatch_micro(node, "deleteAccess", input)
}
