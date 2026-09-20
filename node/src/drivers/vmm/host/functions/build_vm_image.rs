use crate::drivers::vmm::prelude::*;

pub(crate) fn host_fn_build_vm_image(input: &JsonValue) -> String {
    let mut packet = input.clone();
    if let JsonValue::Object(map) = &mut packet {
        map.insert(
            "type".to_string(),
            JsonValue::String("buildVmImage".to_string()),
        );
    }
    crate::drivers::vmm::dispatch_packet(&packet)
}
