//! `aseman-vmm-backend-native LISTEN CONFIG`: serve the native backend over A504 on a
//! loopback address. CONFIG is JSON:
//! `{"state_dir": "...", "node_ca": "path/to/node-ca.pem", "guest_timeout_millis": 30000}`.
//! The runtime plugins read their own settings (`ASEMAN_LEGACY_DOCKER_HOST_GATEWAY_PORT`,
//! Firecracker, Modal, ...) from the environment through `aseman-config`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aseman_config::read_json_file;
use aseman_guest_http::client::GuestApiClient;
use aseman_vmm_backend_grpc::server::BackendService;
use aseman_vmm_backend_native::backend::NativeBackend;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    state_dir: PathBuf,
    node_ca: PathBuf,
    #[serde(default = "default_timeout")]
    guest_timeout_millis: u64,
}

fn default_timeout() -> u64 {
    30_000
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let address: SocketAddr = arguments
        .next()
        .ok_or("the listen address is required")?
        .parse()?;
    if !address.ip().is_loopback() {
        return Err("A504 is served on a loopback address only".into());
    }
    let config: Config = read_json_file(
        arguments
            .next()
            .ok_or("the configuration path is required")?,
    )?;
    if arguments.next().is_some() {
        return Err("unexpected arguments".into());
    }
    let guest = GuestApiClient::new(
        &std::fs::read(&config.node_ca)?,
        Duration::from_millis(config.guest_timeout_millis),
    )?;
    let backend = Arc::new(NativeBackend::start(
        config.state_dir,
        guest,
        // The runtime setting the docker plugin advertises to its containers.
        Some(aseman_config::runtime_config().docker_gateway_port).filter(|port| *port > 0),
    )?);
    tokio::runtime::Runtime::new()?.block_on(async move {
        tonic::transport::Server::builder()
            .add_service(BackendService::new(backend).into_server())
            .serve_with_shutdown(address, async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
    })?;
    Ok(())
}
