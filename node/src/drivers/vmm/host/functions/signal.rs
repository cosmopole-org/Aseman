use crate::drivers::vmm::host::functions::protocol_api::forward_host_api_packet;
use crate::drivers::vmm::prelude::*;

pub(crate) fn host_fn_signal(input: &JsonValue) -> String {
    let signal_type = input["type"].as_str().unwrap_or("").trim();
    let machine_id = input["machineId"].as_str().unwrap_or("").trim();
    if signal_type.is_empty() || machine_id.is_empty() {
        return json!({"ok": false, "error": "machineId and type are required for signal"})
            .to_string();
    }

    forward_host_api_packet("signal", input)
}
