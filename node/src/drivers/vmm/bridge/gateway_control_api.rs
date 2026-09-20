use crate::drivers::vmm::network::gateway::VmGatewayService;
use crate::drivers::vmm::prelude::*;

pub(crate) fn handle_gateway_control_api(packet: &JsonValue) -> Result<JsonValue, String> {
    let api = packet["api"].as_str().unwrap_or("");
    match api {
        "ping" => Ok(json!({"ok": true, "pong": true})),
        "vmLog" => Ok(json!({"ok": true})),
        "gateway.registerEndpoint" => VmGatewayService::register_endpoint(&packet["input"]),
        "gateway.unregisterEndpoint" => VmGatewayService::unregister_endpoint(&packet["input"]),
        "gateway.forward" => VmGatewayService::forward(&packet["input"]),
        "gateway.listEndpoints" => Ok(VmGatewayService::list_endpoints()),
        _ => Err(format!("unsupported bridge api: {}", api)),
    }
}
