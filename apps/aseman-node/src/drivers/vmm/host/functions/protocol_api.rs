use crate::drivers::vmm::prelude::*;

/// The keyed host-API packets these host calls used to post went to the Go-era
/// callback protocol, which carries no caller identity; since LD-14 it served none
/// of them, and the protocol is gone with the embedded VMM (P5-06).
pub(crate) fn forward_host_api_packet(key: &str, _input: &JsonValue) -> String {
    json!({"ok": false, "error": format!("{key} needs an identified caller")}).to_string()
}
