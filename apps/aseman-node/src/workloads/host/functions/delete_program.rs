use crate::adapters::vmm::host::functions::protocol_api::forward_host_api_packet;
use crate::adapters::vmm::prelude::*;

pub(crate) fn host_fn_delete_program(input: &JsonValue) -> String {
    forward_host_api_packet("deleteProgram", input)
}
