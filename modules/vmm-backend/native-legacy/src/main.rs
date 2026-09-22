//! `aseman-vmm-backend-native LISTEN CONFIG`: serve the native backend over A504 on a
//! loopback address. CONFIG is JSON:
//! `{"state_dir": "...", "node_ca": "path/to/node-ca.pem", "guest_timeout_millis": 30000,
//! "docker_gateway_port": 7000}`.

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
    /// The docker-host gateway port docker containers dial; none when unset.
    #[serde(default)]
    docker_gateway_port: Option<u16>,
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
        config.docker_gateway_port,
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
