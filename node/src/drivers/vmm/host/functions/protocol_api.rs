use crate::drivers::vmm::bridge::runtime_io::wasm_send;
use crate::drivers::vmm::prelude::*;

pub(crate) fn forward_host_api_packet(key: &str, input: &JsonValue) -> String {
    let packet = json!({
        "key": key,
        "input": input
    });
    wasm_send(packet)
}
