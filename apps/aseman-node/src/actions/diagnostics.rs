//! Node diagnostics and identity: `/api/hello`, `/api/ping`, `/api/time`, and the
//! node's public key and peers (`node.diagnostics.read`, `node.identity.read`,
//! `node.peers.read`).

use anyhow::Result;
use aseman_application::{Diagnostics, GetServerPeers, GetServerPublicKey};
use aseman_ports::{PeerDirectoryPort, PortResult, ServerIdentityPort};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Ctx;
use crate::workloads::vmm::SystemClock;

#[derive(Debug, Default, Deserialize)]
pub(super) struct HelloInput {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct Empty {}

fn diagnostics<'a>(ctx: &'a Ctx<'_>, port: &'a str) -> Diagnostics<'a> {
    let _ = ctx;
    Diagnostics {
        clock: &SystemClock,
        advertised_port: port,
    }
}

pub(super) fn hello(ctx: &Ctx<'_>, input: HelloInput) -> Result<Value> {
    Ok(json!({"message": diagnostics(ctx, "").hello(&input.name)}))
}

pub(super) fn time(ctx: &Ctx<'_>, _: Empty) -> Result<Value> {
    Ok(json!({"time": diagnostics(ctx, "").time_millis()}))
}

/// Reports the node's configured main port.
pub(super) fn ping(ctx: &Ctx<'_>, _: Empty) -> Result<Value> {
    let port = ctx.node.advertised_port();
    Ok(json!(diagnostics(ctx, &port).ping()))
}

/// The node's identity and peers, as the application's ports see them.
struct NodeIdentity<'a>(&'a Ctx<'a>);

impl ServerIdentityPort for NodeIdentity<'_> {
    fn server_public_key(&self) -> PortResult<String> {
        let pair = self.0.node.tools().security().fetch_key_pair("server_key");
        Ok(pair
            .get(1)
            .map(|key| String::from_utf8_lossy(key).into_owned())
            .unwrap_or_default())
    }
}

impl PeerDirectoryPort for NodeIdentity<'_> {
    fn peer_servers(&self) -> PortResult<Vec<String>> {
        Ok(self.0.node.tools().network().chain().peers())
    }
}

pub(super) fn server_public_key(ctx: &Ctx<'_>, _: Empty) -> Result<Value> {
    let public_key = GetServerPublicKey {
        identity: &NodeIdentity(ctx),
    }
    .execute()?;
    Ok(json!({"publicKey": public_key}))
}

pub(super) fn servers_map(ctx: &Ctx<'_>, _: Empty) -> Result<Value> {
    let servers = GetServerPeers {
        peers: &NodeIdentity(ctx),
    }
    .execute()?;
    Ok(json!({"servers": servers}))
}
