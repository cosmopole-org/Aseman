use crate::drivers::vmm::host::functions::vm_calls::remote_vm_call;
use crate::drivers::vmm::prelude::*;

pub(crate) fn host_fn_copy_from_vm(caller: &str, input: &JsonValue) -> String {
    remote_vm_call("copyFromVm", caller, input)
}
