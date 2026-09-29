//! The VM host calls (`runVm`, `terminateVm`, `execVm`, …) served by the node's VMM
//!: the node resolved and authorized the caller; the VMM does the work.

use crate::workloads::prelude::*;

/// Run VM host call `op` for `caller` (the node-resolved calling program).
pub(crate) fn remote_vm_call(
    node: &Arc<Node>,
    op: &str,
    caller: &str,
    input: &JsonValue,
) -> String {
    let Some(remote) = node.vmm() else {
        return json!({"ok": false, "error": "this node has no VMM (ASEMAN_VMM_ENDPOINT)"})
            .to_string();
    };
    remote.vm_host_call(node, op, caller, input)
}
