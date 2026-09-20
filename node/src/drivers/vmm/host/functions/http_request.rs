use crate::drivers::vmm::host::vm_host_functions::perform_http_request;
use crate::drivers::vmm::prelude::*;

pub(crate) fn host_fn_http_request(input: &JsonValue) -> String {
    match perform_http_request(input) {
        Ok(res) => res,
        Err(err) => json!({"ok": false, "error": err}).to_string(),
    }
}
