use std::sync::Arc;

use serde_json::{Value as JsonValue, json};

use crate::node::Node;

/// `lockResource`: take the advisory lock on `resourceId` for `ownerId`, waiting
/// in line (FIFO) while another owner holds it.
pub(crate) fn host_fn_lock_resource(node: &Arc<Node>, input: &JsonValue) -> String {
    let resource_id = input["resourceId"].as_str().unwrap_or("");
    let owner_id = input["ownerId"].as_str().unwrap_or("");
    match node
        .tools()
        .workloads()
        .acquire_resource_lock(resource_id, owner_id)
    {
        Ok(()) => json!({"ok": true}).to_string(),
        Err(e) => json!({"ok": false, "error": e}).to_string(),
    }
}

/// `unlockResource`: release the lock `ownerId` holds on `resourceId`.
pub(crate) fn host_fn_unlock_resource(node: &Arc<Node>, input: &JsonValue) -> String {
    let resource_id = input["resourceId"].as_str().unwrap_or("");
    let owner_id = input["ownerId"].as_str().unwrap_or("");
    match node
        .tools()
        .workloads()
        .release_resource_lock(resource_id, owner_id)
    {
        Ok(()) => json!({"ok": true}).to_string(),
        Err(e) => json!({"ok": false, "error": e}).to_string(),
    }
}
