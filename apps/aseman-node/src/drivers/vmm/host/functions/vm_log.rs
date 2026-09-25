use serde_json::{Value as JsonValue, json};

/// A guest log line that reached the node. A workload's logs are its VMM's (A501
/// `logs`); a line sent to the node is recorded in the node's own log.
pub(crate) fn host_fn_vm_log(input: &JsonValue) -> String {
    let text = input["text"].as_str().unwrap_or("");
    let vm_id = input["vmId"].as_str().unwrap_or("");
    let log_type = input["logType"].as_str().unwrap_or("runtime");
    eprintln!("[guest {vm_id} {log_type}] {text}");
    json!({"ok": true}).to_string()
}
