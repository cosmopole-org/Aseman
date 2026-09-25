//! Host-local A603 worker-agent executable.

use std::collections::BTreeMap;
use std::io::BufReader;
use std::sync::Arc;

use aseman_config::{read_json_file, read_secret_file};
use aseman_domain::agent::MachineProfile;
use aseman_vmm_agent::host::{Agent, HostConfig};
use aseman_vmm_agent::server::{AgentHttpState, GrantVerifier, ServerTls, serve};
use base64::Engine;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    listen: String,
    allocation_root: std::path::PathBuf,
    firecracker: std::path::PathBuf,
    firecracker_enabled: bool,
    profiles: Vec<MachineProfile>,
    tls_certificate: String,
    tls_key_secret: String,
    client_ca: String,
    /// `name=sha256hex`, one per admitted local backend certificate.
    clients: Vec<String>,
    vmm_key_epoch: u64,
    /// Secret file containing the unpadded-base64url raw Ed25519 public key.
    vmm_public_key_secret: String,
    #[serde(default = "default_body_limit")]
    max_body_bytes: usize,
}

fn default_body_limit() -> usize {
    64 * 1024
}

type Failure = Box<dyn std::error::Error>;

fn certificates(path: &str) -> Result<Vec<CertificateDer<'static>>, Failure> {
    let bytes = std::fs::read(path)?;
    Ok(rustls_pemfile::certs(&mut BufReader::new(bytes.as_slice())).collect::<Result<_, _>>()?)
}

fn private_key(secret: &str) -> Result<PrivateKeyDer<'static>, Failure> {
    let pem = read_secret_file(secret, 64 * 1024)?;
    rustls_pemfile::private_key(&mut BufReader::new(pem.as_bytes()))?
        .ok_or_else(|| "the agent TLS key secret holds no private key".into())
}

fn clients(entries: &[String]) -> Result<BTreeMap<[u8; 32], String>, Failure> {
    entries
        .iter()
        .map(|entry| {
            let (name, digest) = entry
                .split_once('=')
                .ok_or("agent clients must be name=sha256hex")?;
            if name.trim().is_empty() {
                return Err("an agent client name is empty".into());
            }
            let digest = hex::decode(digest)?;
            let digest: [u8; 32] = digest
                .try_into()
                .map_err(|_| "an agent client fingerprint is not SHA-256")?;
            Ok((digest, name.trim().to_owned()))
        })
        .collect()
}

fn main() -> Result<(), Failure> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: aseman-vmm-agent CONFIG.json")?;
    let config: Config = read_json_file(path)?;
    let address: std::net::SocketAddr = config.listen.parse()?;
    if !address.ip().is_loopback() {
        return Err("the A603 agent listener must be host-local/loopback".into());
    }
    let profiles = config
        .profiles
        .into_iter()
        .map(|profile| (profile.name.clone(), profile))
        .collect();
    let agent = Arc::new(Agent::new(HostConfig {
        root: config.allocation_root,
        firecracker: config.firecracker,
        profiles,
        firecracker_enabled: config.firecracker_enabled,
    }));
    let public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(read_secret_file(&config.vmm_public_key_secret, 1024)?.trim())?;
    if public_key.len() != 32 {
        return Err("the VMM Ed25519 public key must be 32 bytes".into());
    }
    let state = Arc::new(AgentHttpState {
        agent,
        verifier: GrantVerifier::new(config.vmm_key_epoch, public_key),
    });
    let tls = ServerTls {
        certificate_chain: certificates(&config.tls_certificate)?,
        private_key: private_key(&config.tls_key_secret)?,
        client_roots: certificates(&config.client_ca)?,
        clients: clients(&config.clients)?,
    };
    tokio::runtime::Runtime::new()?.block_on(async {
        let listener = tokio::net::TcpListener::bind(address).await?;
        serve(listener, tls, state, config.max_body_bytes, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(std::io::Error::other)
    })?;
    Ok(())
}
