use crate::drivers::vmm::globals::with_global_app;
use serde_json::{json, Value as JsonValue};

fn acquire_resource_lock(resource_id: &str, owner_id: &str) -> Result<(), String> {
    match with_global_app(|app| {
        app.tools()
            .vmm()
            .acquire_resource_lock(resource_id, owner_id)
    }) {
        Some(result) => result,
        None => Err("vmm not initialised".to_string()),
    }
}

fn release_resource_lock(resource_id: &str, owner_id: &str) -> Result<(), String> {
    match with_global_app(|app| {
        app.tools()
            .vmm()
            .release_resource_lock(resource_id, owner_id)
    }) {
        Some(result) => result,
        None => Err("vmm not initialised".to_string()),
    }
}

pub(crate) fn host_fn_lock_resource(input: &JsonValue) -> String {
    let resource_id = input["resourceId"].as_str().unwrap_or("");
    let owner_id = input["ownerId"].as_str().unwrap_or("");
    match acquire_resource_lock(resource_id, owner_id) {
        Ok(()) => json!({"ok": true}).to_string(),
        Err(e) => json!({"ok": false, "error": e}).to_string(),
    }
}

pub(crate) fn host_fn_unlock_resource(input: &JsonValue) -> String {
    let resource_id = input["resourceId"].as_str().unwrap_or("");
    let owner_id = input["ownerId"].as_str().unwrap_or("");
    match release_resource_lock(resource_id, owner_id) {
        Ok(()) => json!({"ok": true}).to_string(),
        Err(e) => json!({"ok": false, "error": e}).to_string(),
    }
}
