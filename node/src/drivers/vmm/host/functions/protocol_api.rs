use crate::drivers::vmm::bridge::runtime_io::wasm_send;
use crate::drivers::vmm::prelude::*;

pub(crate) fn forward_host_api_packet(key: &str, input: &JsonValue) -> String {
    let packet = json!({
        "key": key,
        "input": input
    });
    wasm_send(packet)
}

pub(crate) fn host_fn_protocol_api(input: &JsonValue) -> String {
    let key = input["apiKey"]
        .as_str()
        .or_else(|| input["endpoint"].as_str())
        .or_else(|| input["key"].as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    if key.is_empty() {
        return json!({"ok": false, "error": "apiKey (or endpoint/key) is required"}).to_string();
    }

    forward_host_api_packet(&key, input)
}
