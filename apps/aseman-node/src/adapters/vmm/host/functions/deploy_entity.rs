use crate::adapters::vmm::host::functions::protocol_api::forward_host_api_packet;
use crate::adapters::vmm::prelude::*;

pub(crate) fn host_fn_deploy_entity(input: &JsonValue) -> String {
    forward_host_api_packet("deployEntity", input)
}
