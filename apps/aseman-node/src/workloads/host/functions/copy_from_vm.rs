use crate::workloads::host::functions::vm_calls::remote_vm_call;
use crate::workloads::prelude::*;

pub(crate) fn host_fn_copy_from_vm(node: &Arc<Node>, caller: &str, input: &JsonValue) -> String {
    remote_vm_call(node, "copyFromVm", caller, input)
}
