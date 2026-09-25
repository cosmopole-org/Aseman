//! Cluster HTTP listener — one port for both the raft RPC surface and the
//! orchestration/admin API that `casparctl cluster …` drives.
//!
//! Raft RPC (instance ↔ instance):
//!   POST /raft/vote      POST /raft/append      POST /raft/snapshot
//!
//! Orchestration (operator / casparctl, and leader-forwarded proposals):
//!   GET  /cluster/ping               liveness + identity (RTT probes)
//!   GET  /cluster/status             raft metrics + peer latency table
//!   GET  /cluster/peers              configured peers
//!   GET  /cluster/nearest            peers sorted by measured RTT
//!   POST /cluster/init               (re)initialize a pristine cluster
//!   POST /cluster/add-peer           {id, addr, region?, voter?, lat?, lon?}
//!   POST /cluster/remove-peer        {id}
//!   POST /cluster/promote            {ids: [..]} → voter membership
//!   GET  /cluster/config             full cluster config JSON
//!   POST /cluster/config             {key, value} — set one dotted key
//!   POST /cluster/config/apply       whole cluster-config document
//!   POST /cluster/propose            internal: apply a forwarded command
//!
//! The listener speaks minimal HTTP/1.1 (the same style as the telemetry
//! server) to stay dependency-free. When `auth_token` is configured every
//! request must present it in `x-caspar-cluster-token`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use anyhow::{Result, anyhow};
use openraft::BasicNode;
use openraft::raft::{AppendEntriesRequest, InstallSnapshotRequest, VoteRequest};
use serde_json::{Value, json};

use super::ClusterService;
use super::command::{ClusterCommand, TypeConfig};
use super::config::PeerConfig;
use crate::drivers::module_admin::ModuleAdministration;

const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;

pub fn start(svc: Arc<ClusterService>) -> Result<()> {
    let listen = svc.config_snapshot().listen_addr;
    let listener =
        TcpListener::bind(&listen).map_err(|e| anyhow!("cluster bind {}: {}", listen, e))?;
    thread::Builder::new()
        .name("cluster-http".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let svc = svc.clone();
                thread::spawn(move || handle_connection(svc, stream));
            }
        })
        .map_err(|e| anyhow!("cluster server spawn: {}", e))?;
    Ok(())
}

pub fn start_module_admin(
    admin: Arc<dyn ModuleAdministration>,
    listen: String,
    auth_token: String,
) -> Result<()> {
    if auth_token.is_empty() {
        return Err(anyhow!(
            "standalone module administration requires an auth token"
        ));
    }
    let listener = TcpListener::bind(&listen)
        .map_err(|error| anyhow!("module admin bind {listen}: {error}"))?;
    thread::Builder::new()
        .name("module-admin-http".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let admin = admin.clone();
                let auth_token = auth_token.clone();
                thread::spawn(move || handle_module_admin_connection(admin, auth_token, stream));
            }
        })
        .map_err(|error| anyhow!("module admin server spawn: {error}"))?;
    Ok(())
}

struct Request {
    method: String,
    path: String,
    token: String,
    body: Vec<u8>,
}

fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut content_length = 0usize;
    let mut token = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            break;
        }
        let lower = line.to_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("x-caspar-cluster-token:") {
            token = line.splitn(2, ':').nth(1).unwrap_or("").trim().to_string();
        }
        if lower.starts_with("authorization:") {
            let value = line.splitn(2, ':').nth(1).unwrap_or("").trim();
            if let Some(value) = value.strip_prefix("Bearer ") {
                token = value.to_owned();
            } else if let Some(value) = value.strip_prefix("bearer ") {
                token = value.to_owned();
            }
        }
    }
    if content_length > MAX_BODY_BYTES {
        return None;
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 && reader.read_exact(&mut body).is_err() {
        return None;
    }
    Some(Request {
        method,
        path,
        token,
        body,
    })
}

fn respond(mut stream: TcpStream, status: u16, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        if status < 400 { "OK" } else { "ERR" },
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn handle_connection(svc: Arc<ClusterService>, stream: TcpStream) {
    let Some(req) = read_request(&stream) else {
        return;
    };
    let expected = svc.auth_token();
    if (req.path == "/v1/admin/modules" || req.path.starts_with("/v1/admin/modules/"))
        && expected.is_empty()
    {
        respond(
            stream,
            503,
            br#"{"error":"module administration requires a configured cluster auth token"}"#,
        );
        return;
    }
    if !expected.is_empty() && req.token != expected {
        respond(stream, 401, br#"{"error":"invalid cluster token"}"#);
        return;
    }
    let (status, body) = route(&svc, &req);
    respond(stream, status, &body);
}

fn handle_module_admin_connection(
    admin: Arc<dyn ModuleAdministration>,
    auth_token: String,
    stream: TcpStream,
) {
    let Some(req) = read_request(&stream) else {
        return;
    };
    if req.token != auth_token {
        respond(stream, 401, br#"{"error":"invalid administration token"}"#);
        return;
    }
    if req.path != "/v1/admin/modules" && !req.path.starts_with("/v1/admin/modules/") {
        respond(stream, 404, br#"{"error":"not found"}"#);
        return;
    }
    match admin.handle(&req.method, &req.path, &req.body) {
        Ok(value) => {
            let (_, body) = json_ok(value);
            respond(stream, 200, &body);
        }
        Err(error) => {
            let (_, body) = json_err(error.status, error.message);
            respond(stream, error.status, &body);
        }
    }
}

fn route(svc: &Arc<ClusterService>, req: &Request) -> (u16, Vec<u8>) {
    if req.path == "/v1/admin/modules" || req.path.starts_with("/v1/admin/modules/") {
        return match svc.module_admin().handle(&req.method, &req.path, &req.body) {
            Ok(value) => json_ok(value),
            Err(error) => json_err(error.status, error.message),
        };
    }
    match (req.method.as_str(), req.path.as_str()) {
        // ── raft RPC ────────────────────────────────────────────────────
        ("POST", "/raft/vote") => raft_rpc(req, |body| {
            let rpc: VoteRequest<u64> = serde_json::from_slice(body)?;
            Ok(svc.runtime().block_on(svc.raft().vote(rpc)))
        }),
        ("POST", "/raft/append") => raft_rpc(req, |body| {
            let rpc: AppendEntriesRequest<TypeConfig> = serde_json::from_slice(body)?;
            Ok(svc.runtime().block_on(svc.raft().append_entries(rpc)))
        }),
        ("POST", "/raft/snapshot") => raft_rpc(req, |body| {
            let rpc: InstallSnapshotRequest<TypeConfig> = serde_json::from_slice(body)?;
            Ok(svc.runtime().block_on(svc.raft().install_snapshot(rpc)))
        }),

        // ── identity / telemetry ────────────────────────────────────────
        ("GET", "/cluster/ping") => {
            let cfg = svc.config_snapshot();
            json_ok(json!({
                "nodeId": svc.node_id,
                "nodeName": cfg.node_name,
                "region": cfg.region,
                "time": chrono::Utc::now().timestamp_millis(),
            }))
        }
        ("GET", "/cluster/status") => json_ok(svc.status_json()),
        ("GET", "/cluster/peers") => {
            let cfg = svc.config_snapshot();
            json_ok(json!({"peers": cfg.peers.values().collect::<Vec<_>>()}))
        }
        ("GET", "/cluster/nearest") => json_ok(json!({"peers": svc.nearest_peers()})),

        // ── membership orchestration ───────────────────────────────────
        ("POST", "/cluster/init") => handle_init(svc, &req.body),
        ("POST", "/cluster/add-peer") => handle_add_peer(svc, &req.body),
        ("POST", "/cluster/remove-peer") => handle_remove_peer(svc, &req.body),
        ("POST", "/cluster/promote") => handle_promote(svc, &req.body),

        // ── configuration ───────────────────────────────────────────────
        ("GET", "/cluster/config") => {
            json_ok(serde_json::to_value(svc.config_snapshot()).unwrap_or(Value::Null))
        }
        ("POST", "/cluster/config") => handle_config_set(svc, &req.body),
        ("POST", "/cluster/config/apply") => handle_config_apply(svc, &req.body),

        // ── forwarded proposals ─────────────────────────────────────────
        ("POST", "/cluster/propose") => {
            let cmd: ClusterCommand = match serde_json::from_slice(&req.body) {
                Ok(c) => c,
                Err(e) => return json_err(400, format!("bad command: {}", e)),
            };
            match svc.propose_blocking(&cmd) {
                Ok(()) => json_ok(json!({"ok": true})),
                Err(e) => json_err(503, e.to_string()),
            }
        }

        _ => json_err(404, "not found".to_string()),
    }
}

fn raft_rpc<T: serde::Serialize>(
    req: &Request,
    f: impl FnOnce(&[u8]) -> Result<T>,
) -> (u16, Vec<u8>) {
    match f(&req.body) {
        Ok(result) => match serde_json::to_vec(&result) {
            Ok(b) => (200, b),
            Err(e) => json_err(500, format!("encode: {}", e)),
        },
        Err(e) => json_err(400, e.to_string()),
    }
}

fn json_ok(v: Value) -> (u16, Vec<u8>) {
    (200, serde_json::to_vec(&v).unwrap_or_default())
}

fn json_err(status: u16, msg: String) -> (u16, Vec<u8>) {
    (
        status,
        serde_json::to_vec(&json!({"error": msg})).unwrap_or_default(),
    )
}

fn body_json(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or_else(|_| json!({}))
}

/// `POST /cluster/init` — initialize a pristine raft with this node plus any
/// peers already registered in the config, forming the initial voter set.
fn handle_init(svc: &Arc<ClusterService>, body: &[u8]) -> (u16, Vec<u8>) {
    let input = body_json(body);
    let cfg = svc.config_snapshot();
    let mut members: BTreeMap<u64, BasicNode> = BTreeMap::new();
    members.insert(svc.node_id, BasicNode::new(cfg.advertise_addr.clone()));
    if input["includePeers"].as_bool().unwrap_or(false) {
        for peer in cfg.peers.values().filter(|p| p.voter) {
            members.insert(peer.id, BasicNode::new(peer.addr.clone()));
        }
    }
    match svc.runtime().block_on(svc.raft().initialize(members)) {
        Ok(()) => json_ok(json!({"ok": true})),
        Err(e) => json_err(409, e.to_string()),
    }
}

/// `POST /cluster/add-peer` — introduce another Caspar instance of the same
/// origin by address. The peer is stored in the local config, added to raft
/// as a learner, and — unless `voter:false` — promoted into the voter set.
fn handle_add_peer(svc: &Arc<ClusterService>, body: &[u8]) -> (u16, Vec<u8>) {
    let input = body_json(body);
    let id = input["id"].as_u64().unwrap_or(0);
    let addr = input["addr"].as_str().unwrap_or("").to_string();
    if id == 0 || addr.is_empty() {
        return json_err(400, "id and addr are required".to_string());
    }
    let peer = PeerConfig {
        id,
        addr: addr.clone(),
        region: input["region"].as_str().unwrap_or("").to_string(),
        latitude: input["latitude"].as_f64(),
        longitude: input["longitude"].as_f64(),
        voter: input["voter"].as_bool().unwrap_or(true),
    };
    let voter = peer.voter;
    if let Err(e) = svc.update_config(|cfg| {
        cfg.peers.insert(id, peer.clone());
    }) {
        return json_err(500, e.to_string());
    }
    let res = svc.runtime().block_on(async {
        svc.raft()
            .add_learner(id, BasicNode::new(addr.clone()), true)
            .await
            .map_err(|e| anyhow!("add_learner: {}", e))?;
        if voter {
            let voters = voter_set_with(svc, Some(id), None);
            svc.raft()
                .change_membership(voters, false)
                .await
                .map_err(|e| anyhow!("change_membership: {}", e))?;
        }
        Ok::<(), anyhow::Error>(())
    });
    match res {
        Ok(()) => json_ok(json!({"ok": true, "added": id, "voter": voter})),
        Err(e) => json_err(503, e.to_string()),
    }
}

/// `POST /cluster/remove-peer` — remove an instance from membership and from
/// the local peer registry.
fn handle_remove_peer(svc: &Arc<ClusterService>, body: &[u8]) -> (u16, Vec<u8>) {
    let input = body_json(body);
    let id = input["id"].as_u64().unwrap_or(0);
    if id == 0 {
        return json_err(400, "id is required".to_string());
    }
    let res = svc.runtime().block_on(async {
        let voters = voter_set_with(svc, None, Some(id));
        svc.raft()
            .change_membership(voters, false)
            .await
            .map_err(|e| anyhow!("change_membership: {}", e))
    });
    if let Err(e) = res {
        return json_err(503, e.to_string());
    }
    if let Err(e) = svc.update_config(|cfg| {
        cfg.peers.remove(&id);
    }) {
        return json_err(500, e.to_string());
    }
    json_ok(json!({"ok": true, "removed": id}))
}

/// `POST /cluster/promote` — set the exact voter membership (`{"ids":[1,2,3]}`).
fn handle_promote(svc: &Arc<ClusterService>, body: &[u8]) -> (u16, Vec<u8>) {
    let input = body_json(body);
    let ids: BTreeSet<u64> = input["ids"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return json_err(400, "ids array is required".to_string());
    }
    match svc
        .runtime()
        .block_on(svc.raft().change_membership(ids.clone(), false))
    {
        Ok(_) => json_ok(json!({"ok": true, "voters": ids})),
        Err(e) => json_err(503, e.to_string()),
    }
}

/// `POST /cluster/config` — set a single dotted config key on this node.
fn handle_config_set(svc: &Arc<ClusterService>, body: &[u8]) -> (u16, Vec<u8>) {
    let input = body_json(body);
    let key = input["key"].as_str().unwrap_or("");
    if key.is_empty() {
        return json_err(400, "key is required".to_string());
    }
    let raw_value = match &input["value"] {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match svc.set_config_key(key, &raw_value) {
        Ok(cfg) => {
            // Free-form knobs replicate cluster-wide through the log.
            if key.starts_with("extra.") {
                svc_propose_config(svc, key, &cfg);
            }
            json_ok(serde_json::to_value(cfg).unwrap_or(Value::Null))
        }
        Err(e) => json_err(400, e.to_string()),
    }
}

fn svc_propose_config(svc: &Arc<ClusterService>, key: &str, cfg: &super::config::ClusterConfig) {
    let bare = key.trim_start_matches("extra.");
    if let Some(value) = cfg.extra.get(bare) {
        let _ = svc.propose_blocking(&ClusterCommand::ConfigPut {
            key: bare.to_string(),
            value: value.clone(),
        });
    }
}

/// `POST /cluster/config/apply` — replace the cluster config with a whole
/// document (the `casparctl cluster apply -f file` path). Peers present in
/// the document are registered and joined one by one.
fn handle_config_apply(svc: &Arc<ClusterService>, body: &[u8]) -> (u16, Vec<u8>) {
    let incoming: super::config::ClusterConfig = match serde_json::from_slice(body) {
        Ok(c) => c,
        Err(e) => return json_err(400, format!("bad cluster config: {}", e)),
    };
    let self_id = svc.node_id;
    let updated = svc.update_config(|cfg| {
        // Identity + listener fields stay under the running node's control;
        // everything else (tunables, peers, extra) comes from the document.
        cfg.heartbeat_interval_ms = incoming.heartbeat_interval_ms;
        cfg.election_timeout_min_ms = incoming.election_timeout_min_ms;
        cfg.election_timeout_max_ms = incoming.election_timeout_max_ms;
        cfg.max_payload_entries = incoming.max_payload_entries;
        cfg.snapshot_logs_since_last = incoming.snapshot_logs_since_last;
        cfg.max_in_snapshot_log_to_keep = incoming.max_in_snapshot_log_to_keep;
        cfg.rtt_probe_interval_secs = incoming.rtt_probe_interval_secs;
        cfg.proposal_queue_capacity = incoming.proposal_queue_capacity;
        cfg.region = incoming.region.clone();
        cfg.latitude = incoming.latitude;
        cfg.longitude = incoming.longitude;
        cfg.extra = incoming.extra.clone();
        for (id, peer) in &incoming.peers {
            if *id != self_id {
                cfg.peers.insert(*id, peer.clone());
            }
        }
    });
    let updated = match updated {
        Ok(u) => u,
        Err(e) => return json_err(500, e.to_string()),
    };

    // Join every configured peer: learners first, then one voter promotion.
    let mut joined: Vec<u64> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let res = svc.runtime().block_on(async {
        for peer in updated.peers.values() {
            if peer.id == self_id {
                continue;
            }
            match svc
                .raft()
                .add_learner(peer.id, BasicNode::new(peer.addr.clone()), true)
                .await
            {
                Ok(_) => joined.push(peer.id),
                Err(e) => errors.push(format!("peer {}: {}", peer.id, e)),
            }
        }
        let voters: BTreeSet<u64> = updated
            .peers
            .values()
            .filter(|p| p.voter && joined.contains(&p.id))
            .map(|p| p.id)
            .chain(std::iter::once(self_id))
            .collect();
        if voters.len() > 1 {
            if let Err(e) = svc.raft().change_membership(voters, false).await {
                errors.push(format!("change_membership: {}", e));
            }
        }
        Ok::<(), anyhow::Error>(())
    });
    if let Err(e) = res {
        errors.push(e.to_string());
    }
    json_ok(json!({"ok": errors.is_empty(), "joined": joined, "errors": errors}))
}

/// Current voter set, optionally adding/removing one id.
fn voter_set_with(
    svc: &Arc<ClusterService>,
    add: Option<u64>,
    remove: Option<u64>,
) -> BTreeSet<u64> {
    let metrics = svc.raft().metrics().borrow().clone();
    let mut voters: BTreeSet<u64> = metrics.membership_config.membership().voter_ids().collect();
    if let Some(id) = add {
        voters.insert(id);
    }
    if let Some(id) = remove {
        voters.remove(&id);
    }
    voters
}
