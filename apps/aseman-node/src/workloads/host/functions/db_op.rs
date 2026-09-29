use crate::workloads::host::vm_host_functions::{HostHierarchy, run_db_op};
use crate::workloads::prelude::*;

pub(crate) fn host_fn_db_op(node: &Arc<Node>, ctx: &HostHierarchy, input: &JsonValue) -> String {
    match run_db_op(node, ctx, input) {
        Ok(res) => res,
        Err(err) => json!({"ok": false, "error": err}).to_string(),
    }
}
