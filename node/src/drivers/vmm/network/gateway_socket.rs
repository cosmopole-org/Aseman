use crate::drivers::vmm::network::gateway_types::{
    GatewayForwardRequest, GatewayProtocol, VmGatewayEndpoint,
};
use crate::drivers::vmm::prelude::*;

pub(crate) fn forward_raw_socket_to_vm(
    endpoint: &VmGatewayEndpoint,
    req: &GatewayForwardRequest,
) -> Result<JsonValue, String> {
    let port = endpoint
        .protocol_port(GatewayProtocol::RawSocket)
        .ok_or_else(|| "missing VM raw socket port in gateway endpoint".to_string())?;
    let addr = format!("{}:{}", endpoint.host, port);

    let mut stream = std::net::TcpStream::connect(&addr)
        .map_err(|e| format!("gateway TCP connect failed ({}): {}", addr, e))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| format!("failed to set socket read timeout: {}", e))?;
    stream
        .write_all(&req.body)
        .map_err(|e| format!("failed to write TCP payload: {}", e))?;

    let mut buf = vec![0_u8; 64 * 1024];
    let sz = stream
        .read(&mut buf)
        .map_err(|e| format!("failed to read TCP response: {}", e))?;
    buf.truncate(sz);

    Ok(json!({
        "ok": true,
        "protocol": "raw_socket",
        "runtime": endpoint.runtime.clone(),
        "machineId": endpoint.machine_id,
        "vmId": endpoint.vm_id,
        "bodyBase64": BASE64_STANDARD.encode(buf),
    }))
}

pub(crate) fn forward_websocket_to_vm(
    endpoint: &VmGatewayEndpoint,
    req: &GatewayForwardRequest,
) -> Result<JsonValue, String> {
    let port = endpoint
        .protocol_port(GatewayProtocol::WebSocket)
        .ok_or_else(|| "missing VM websocket port in gateway endpoint".to_string())?;

    let addr = format!("{}:{}", endpoint.host, port);
    let mut stream = std::net::TcpStream::connect(&addr)
        .map_err(|e| format!("gateway websocket TCP connect failed ({}): {}", addr, e))?;

    stream
        .write_all(&req.body)
        .map_err(|e| format!("failed to write websocket frame payload: {}", e))?;

    let mut buf = vec![0_u8; 64 * 1024];
    let sz = stream
        .read(&mut buf)
        .map_err(|e| format!("failed to read websocket frame payload: {}", e))?;
    buf.truncate(sz);

    Ok(json!({
        "ok": true,
        "protocol": "websocket",
        "runtime": endpoint.runtime.clone(),
        "machineId": endpoint.machine_id,
        "vmId": endpoint.vm_id,
        "bodyBase64": BASE64_STANDARD.encode(buf),
    }))
}
