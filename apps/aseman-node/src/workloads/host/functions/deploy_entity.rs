use crate::workloads::host::functions::protocol_api::forward_host_api_packet;
use crate::workloads::prelude::*;

pub(crate) fn host_fn_deploy_entity(input: &JsonValue) -> String {
    forward_host_api_packet("deployEntity", input)
}
