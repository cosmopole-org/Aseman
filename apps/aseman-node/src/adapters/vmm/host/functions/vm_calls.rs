//! The VM host calls (`runVm`, `terminateVm`, `execVm`, …) served by the node's VMM
//! (P5-06): the node resolved and authorized the caller; the VMM does the work.

use crate::drivers::vmm::globals::with_global_app;
use crate::drivers::vmm::prelude::*;

/// Run VM host call `op` for `caller` (the node-resolved calling program).
pub(crate) fn remote_vm_call(op: &str, caller: &str, input: &JsonValue) -> String {
    let (Some(remote), Some(app)) = (
        crate::shell::workloads::remote(),
        with_global_app(|app| app.clone()),
    ) else {
        return json!({"ok": false, "error": "this node has no VMM (ASEMAN_VMM_ENDPOINT)"})
            .to_string();
    };
    remote.vm_host_call(&app, op, caller, input)
}
