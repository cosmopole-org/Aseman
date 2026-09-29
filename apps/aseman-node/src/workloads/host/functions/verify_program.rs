use crate::workloads::prelude::*;

/// Verify a program execution proof with the node's VMM (the runtime that proves
/// executions verifies them).
pub(crate) fn host_fn_verify_program(node: &Arc<Node>, input: &JsonValue) -> String {
    match node.vmm() {
        Some(remote) => remote.verify_execution(input),
        None => {
            json!({"ok": false, "error": "this node has no VMM (ASEMAN_VMM_ENDPOINT)"}).to_string()
        }
    }
}
