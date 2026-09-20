use crate::drivers::vmm::prelude::*;

pub(crate) fn host_fn_terminate_vm(input: &JsonValue) -> String {
    let mut packet = input.clone();
    if let JsonValue::Object(map) = &mut packet {
        map.insert(
            "type".to_string(),
            JsonValue::String("terminateVm".to_string()),
        );
    }
    crate::drivers::vmm::dispatch_packet(&packet)
}
