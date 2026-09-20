use crate::drivers::vmm::network::gateway_http::forward_http_to_vm;
use crate::drivers::vmm::network::gateway_registry::VmGatewayRegistry;
use crate::drivers::vmm::network::gateway_socket::{
    forward_raw_socket_to_vm, forward_websocket_to_vm,
};
use crate::drivers::vmm::network::gateway_types::{
    GatewayForwardRequest, GatewayProtocol, VmGatewayEndpoint,
};
use crate::drivers::vmm::prelude::*;

pub(crate) struct VmGatewayService;

impl VmGatewayService {
    pub(crate) fn register_endpoint(packet: &JsonValue) -> Result<JsonValue, String> {
        let runtime =
            caspar_vm_sdk::registry::resolve_key(packet["runtime"].as_str().unwrap_or(""))
                .ok_or_else(|| {
                    format!(
                        "runtime is required and must be one of the registered VM types: {}",
                        caspar_vm_sdk::registry::keys().join("/")
                    )
                })?;
        let machine_id = packet["machineId"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        let vm_id = packet["vmId"].as_str().unwrap_or("main").trim().to_string();
        let host = packet["host"]
            .as_str()
            .unwrap_or("127.0.0.1")
            .trim()
            .to_string();
        if machine_id.is_empty() {
            return Err("machineId is required".to_string());
        }
        if vm_id.is_empty() {
            return Err("vmId is required".to_string());
        }

        let endpoint = VmGatewayEndpoint {
            machine_id: machine_id.clone(),
            vm_id: vm_id.clone(),
            runtime,
            host,
            http_port: packet["httpPort"].as_u64().map(|x| x as u16),
            websocket_port: packet["websocketPort"].as_u64().map(|x| x as u16),
            raw_socket_port: packet["rawSocketPort"].as_u64().map(|x| x as u16),
        };
        VmGatewayRegistry::register(endpoint);

        Ok(json!({"ok": true, "machineId": machine_id, "vmId": vm_id}))
    }

    pub(crate) fn unregister_endpoint(packet: &JsonValue) -> Result<JsonValue, String> {
        let machine_id = packet["machineId"].as_str().unwrap_or("").trim();
        let vm_id = packet["vmId"].as_str().unwrap_or("main").trim();
        if machine_id.is_empty() {
            return Err("machineId is required".to_string());
        }
        VmGatewayRegistry::unregister(machine_id, vm_id);
        Ok(json!({"ok": true, "machineId": machine_id, "vmId": vm_id}))
    }

    pub(crate) fn list_endpoints() -> JsonValue {
        let endpoints = VmGatewayRegistry::list();
        json!({"ok": true, "endpoints": endpoints.iter().map(|e| json!({
            "machineId": e.machine_id,
            "vmId": e.vm_id,
            "runtime": e.runtime,
            "host": e.host,
            "httpPort": e.http_port,
            "websocketPort": e.websocket_port,
            "rawSocketPort": e.raw_socket_port,
        })).collect::<Vec<JsonValue>>()})
    }

    pub(crate) fn forward(packet: &JsonValue) -> Result<JsonValue, String> {
        let protocol = GatewayProtocol::from_str(packet["protocol"].as_str().unwrap_or(""))
            .ok_or_else(|| {
                "protocol is required and must be http/websocket/raw_socket".to_string()
            })?;
        let machine_id = packet["machineId"].as_str().unwrap_or("");
        let vm_id = packet["vmId"].as_str().unwrap_or("main");
        let endpoint = VmGatewayRegistry::resolve(machine_id, vm_id).ok_or_else(|| {
            format!(
                "no gateway endpoint registered for {}::{}",
                machine_id, vm_id
            )
        })?;

        let req = GatewayForwardRequest {
            machine_id: machine_id.to_string(),
            vm_id: vm_id.to_string(),
            protocol,
            path: packet["path"].as_str().unwrap_or("/").to_string(),
            method: packet["method"].as_str().unwrap_or("GET").to_string(),
            body: packet["bodyBase64"]
                .as_str()
                .map(|s| BASE64_STANDARD.decode(s).unwrap_or_default())
                .unwrap_or_default(),
            headers: packet["headers"]
                .as_object()
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|sv| (k.clone(), sv.to_string())))
                        .collect::<HashMap<String, String>>()
                })
                .unwrap_or_default(),
        };

        match protocol {
            GatewayProtocol::Http => forward_http_to_vm(&endpoint, &req),
            GatewayProtocol::WebSocket => forward_websocket_to_vm(&endpoint, &req),
            GatewayProtocol::RawSocket => forward_raw_socket_to_vm(&endpoint, &req),
        }
    }
}
