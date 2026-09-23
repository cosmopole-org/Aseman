//! `aseman-vmm-backend-nomad LISTEN CONFIG`: serve the Nomad backend over A504 on a
//! loopback address. CONFIG is JSON:
//! `{"endpoint": "http://127.0.0.1:4646", "namespace": "aseman",
//!   "datacenters": ["dc1"], "token_file": "...", "runtimes": {"docker": {}},
//!   "network": {"cni": "aseman-restricted", "denies_egress": true},
//!   "timeout_millis": 30000}`.
//!
//! A runtime entry with a `runner_image` runs in that hardened image; one without
//! runs the workload's own artifact image.
//!
//! `network` names the network allocations are placed on. Nomad's plain `bridge`
//! gives unrestricted egress, so a workload whose policy denies egress is refused
//! there: the operator must supply a CNI network that enforces it (A406, A602).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use aseman_config::{read_json_file, read_secret_file};
use aseman_vmm_backend_grpc::server::BackendService;
use aseman_vmm_backend_nomad::backend::{NomadBackend, Runtime, runtime_capabilities};
use aseman_vmm_backend_nomad::client::Nomad;
use aseman_vmm_backend_nomad::job::{Execution, NetworkMode};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeConfig {
    /// The hardened image this runtime runs in; absent means the workload's own.
    #[serde(default)]
    runner_image: Option<String>,
}

/// The network allocations are placed on.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkConfig {
    /// A CNI network name, without the `cni/` prefix. Absent means Nomad's plain
    /// bridge, which cannot deny egress.
    #[serde(default)]
    cni: Option<String>,
    /// Whether the named network denies egress by default. The operator declares
    /// this because only they know what the network does; the backend refuses a
    /// deny-by-default workload when it is false.
    #[serde(default)]
    denies_egress: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    endpoint: String,
    #[serde(default = "default_namespace")]
    namespace: String,
    #[serde(default = "default_datacenters")]
    datacenters: Vec<String>,
    /// A file holding the Nomad ACL token; absent means an unauthenticated cluster.
    #[serde(default)]
    token_file: Option<String>,
    runtimes: std::collections::BTreeMap<String, RuntimeConfig>,
    #[serde(default)]
    network: NetworkConfig,
    #[serde(default = "default_timeout")]
    timeout_millis: u64,
}

fn default_namespace() -> String {
    "aseman".to_owned()
}

fn default_datacenters() -> Vec<String> {
    vec!["dc1".to_owned()]
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
    if config.runtimes.is_empty() {
        return Err("the backend must offer at least one runtime".into());
    }
    let token = config
        .token_file
        .as_deref()
        .map(|path| read_secret_file(path, 4096))
        .transpose()?
        .map(|text| text.trim().to_owned());
    let nomad = Nomad::new(
        &config.endpoint,
        &config.namespace,
        token,
        Duration::from_millis(config.timeout_millis),
    )?;
    let runtimes = config
        .runtimes
        .into_iter()
        .map(|(key, runtime)| {
            let execution = match runtime.runner_image {
                Some(image) => Execution::Runner(image),
                None => Execution::Image,
            };
            Ok(Runtime {
                capabilities: runtime_capabilities(&key, &execution)?,
                execution,
            })
        })
        .collect::<Result<Vec<_>, aseman_ports::PortError>>()?;
    let network = match config.network.cni {
        Some(name) => NetworkMode::Cni {
            name,
            denies_egress: config.network.denies_egress,
        },
        None => NetworkMode::Bridge,
    };
    let backend = Arc::new(NomadBackend::start(
        nomad,
        runtimes,
        config.datacenters,
        network,
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
