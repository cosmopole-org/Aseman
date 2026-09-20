use crate::drivers::vmm::host::functions::protocol_api::forward_host_api_packet;
use crate::drivers::vmm::prelude::*;

pub(crate) fn host_fn_validate_sign(input: &JsonValue) -> String {
    forward_host_api_packet("validateSign", input)
}
