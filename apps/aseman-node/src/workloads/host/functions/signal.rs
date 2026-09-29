use crate::workloads::prelude::*;

pub(crate) fn host_fn_signal(node: &Arc<Node>, input: &JsonValue) -> String {
    let signal_type = input["type"].as_str().unwrap_or("").trim();
    let machine_id = input["machineId"].as_str().unwrap_or("").trim();
    if signal_type.is_empty() || machine_id.is_empty() {
        return json!({"ok": false, "error": "machineId and type are required for signal"})
            .to_string();
    }

    node.tools().workloads().host_action_signal(input)
}
