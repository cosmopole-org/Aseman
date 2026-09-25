use crate::drivers::vmm::host::functions::vm_calls::remote_vm_call;
use crate::drivers::vmm::prelude::*;

pub(crate) fn host_fn_exec_vm(caller: &str, input: &JsonValue) -> String {
    remote_vm_call("execVm", caller, input)
}
