//! Geo-distributed Caspar cluster — edge-style multi-instance replication.
//!
//! A Caspar deployment can now run as a set of instances of the same origin
//! (authority) spread across the globe. Requests are served by whichever
//! instance receives them (lowest geographic latency, like an edge network);
//! the resulting state changes are then propagated to every other instance
//! through an OpenRaft-replicated log:
//!
//! * every **shell API** write commits locally and its write-set is proposed
//!   to the raft log, so the shell surface is consistent on all instances;
//! * a creature **deployed with `distribution: "cluster"`** is shipped —
//!   artifact bytes included — to every instance, which can then execute its
//!   VM locally and propagate the execution results (storage mutations,
//!   transactions) back through the same log;
//! * a creature deployed with `distribution: "local"` (the default) stays on
//!   the receiving instance and none of its VM state enters the consensus.
//!
//! Orchestration (introducing peers one by one or from a cluster config
//! file, tuning every knob individually) is exposed over the cluster HTTP
//! listener and driven by `casparctl cluster …`.

pub mod command;
pub mod config;
pub mod network;
pub mod server;
pub mod store;

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use aseman_config::ClusterBootstrapConfig;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use once_cell::sync::OnceCell;
use openraft::{BasicNode, Raft};
use serde_json::{json, Value};

use crate::drivers::module_admin::{ModuleAdminService, ModuleAdministration};
use crate::models::core::ICore;
use crate::models::transaction::ITrx;

use command::{ClusterCommand, DeployArtifact, KvOp, TypeConfig};
use config::{ClusterConfig, PeerConfig};
use store::{CommandApplier, LogStore, StateMachineStore};

// ────────────────────────── Global handle ──────────────────────────────────

static CLUSTER: OnceCell<Arc<ClusterService>> = OnceCell::new();

thread_local! {
    /// True while this thread is applying a committed raft entry — local
    /// commits made during apply must not be re-proposed.
    static APPLYING: Cell<bool> = const { Cell::new(false) };
    /// Per-thread replication scope; `false` marks the section as local-only
    /// (used around non-distributed VM commits).
    static REPLICATE_SCOPE: Cell<bool> = const { Cell::new(true) };
}

/// Fetch the running cluster service, if this node is cluster-enabled.
pub fn service() -> Option<Arc<ClusterService>> {
    CLUSTER.get().cloned()
}

pub fn is_active() -> bool {
    CLUSTER.get().is_some()
}

/// Run `f` with replication switched on/off for the current thread. Used by
/// the VMM to keep non-distributed VM mutations out of the consensus while
/// letting distributed VM mutations through.
pub fn with_replication_scope<R>(replicate: bool, f: impl FnOnce() -> R) -> R {
    REPLICATE_SCOPE.with(|s| {
        let prev = s.get();
        s.set(replicate);
        let out = f();
        s.set(prev);
        out
    })
}

fn with_applying_guard<R>(f: impl FnOnce() -> R) -> R {
    APPLYING.with(|a| {
        let prev = a.get();
        a.set(true);
        let out = f();
        a.set(prev);
        out
    })
}

/// Should the write-set of a transaction committing on this thread be
/// proposed to the cluster? Consulted by `TrxWrapper::commit`.
pub fn should_replicate() -> bool {
    if CLUSTER.get().is_none() {
        return false;
    }
    if APPLYING.with(|a| a.get()) {
        return false;
    }
    REPLICATE_SCOPE.with(|s| s.get())
}

/// Hand a locally committed write-set to the cluster for propagation.
/// Non-blocking: the batch is queued and proposed by the replication worker.
pub fn on_local_commit(ops: Vec<KvOp>) {
    if ops.is_empty() {
        return;
    }
    if let Some(svc) = service() {
        svc.enqueue(ClusterCommand::KvBatch {
            origin: svc.node_id,
            ops,
        });
    }
}

/// Propagate a distributed creature deployment to every instance.
pub fn propose_deploy(artifact: DeployArtifact) {
    if let Some(svc) = service() {
        svc.enqueue(ClusterCommand::Deploy {
            origin: svc.node_id,
            artifact,
        });
    }
}

// ────────────────────────── Service ────────────────────────────────────────

/// Peer latency book-keeping for edge routing decisions.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PeerHealth {
    pub node_id: u64,
    pub addr: String,
    pub region: String,
    /// Exponentially-weighted RTT in milliseconds; `None` until first probe.
    pub rtt_ms: Option<f64>,
    pub reachable: bool,
}

pub struct ClusterService {
    pub node_id: u64,
    app: Arc<dyn ICore>,
    raft: Raft<TypeConfig>,
    rt: tokio::runtime::Runtime,
    config: RwLock<ClusterConfig>,
    config_path: PathBuf,
    proposal_tx: crossbeam_channel::Sender<ClusterCommand>,
    rtt: RwLock<HashMap<u64, PeerHealth>>,
    dropped_proposals: AtomicU64,
    module_admin: Arc<dyn ModuleAdministration>,
}

impl ClusterService {
    pub fn raft(&self) -> &Raft<TypeConfig> {
        &self.raft
    }

    pub fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.rt
    }

    pub fn config_snapshot(&self) -> ClusterConfig {
        self.config.read().unwrap().clone()
    }

    pub fn auth_token(&self) -> String {
        self.config.read().unwrap().auth_token.clone()
    }

    pub fn module_admin(&self) -> &dyn ModuleAdministration {
        self.module_admin.as_ref()
    }

    /// Persist a config mutation and return the updated copy.
    pub fn update_config(&self, f: impl FnOnce(&mut ClusterConfig)) -> Result<ClusterConfig> {
        let mut cfg = self.config.write().unwrap();
        f(&mut cfg);
        cfg.save(&self.config_path)?;
        Ok(cfg.clone())
    }

    /// Set one dotted config key (`casparctl cluster config set k v`).
    pub fn set_config_key(&self, key: &str, raw_value: &str) -> Result<ClusterConfig> {
        let mut cfg = self.config.write().unwrap();
        let updated = cfg.set_key(key, raw_value)?;
        *cfg = updated.clone();
        cfg.save(&self.config_path)?;
        Ok(updated)
    }

    fn enqueue(&self, cmd: ClusterCommand) {
        if self.proposal_tx.try_send(cmd).is_err() {
            let n = self.dropped_proposals.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 100 == 1 {
                eprintln!(
                    "[cluster] replication queue full — {} proposals dropped so far",
                    n
                );
            }
        }
    }

    /// Propose a command, forwarding to the current leader when this
    /// instance is a follower. Blocking; used by the replication worker and
    /// the admin `/cluster/propose` endpoint.
    pub fn propose_blocking(&self, cmd: &ClusterCommand) -> Result<()> {
        let metrics = self.raft.metrics().borrow().clone();
        let leader = metrics.current_leader;
        if leader == Some(self.node_id) {
            self.rt
                .block_on(self.raft.client_write(cmd.clone()))
                .map_err(|e| anyhow!("raft client_write: {}", e))?;
            return Ok(());
        }
        let Some(leader_id) = leader else {
            return Err(anyhow!("no raft leader elected yet"));
        };
        let leader_addr = metrics
            .membership_config
            .membership()
            .get_node(&leader_id)
            .map(|n| n.addr.clone())
            .ok_or_else(|| anyhow!("leader {} has no known address", leader_id))?;
        let token = self.auth_token();
        let body = serde_json::to_vec(cmd)?;
        let url = format!("http://{}/cluster/propose", leader_addr);
        self.rt.block_on(async {
            let client = reqwest::Client::new();
            let mut builder = client
                .post(&url)
                .timeout(Duration::from_secs(30))
                .header("content-type", "application/json")
                .body(body);
            if !token.is_empty() {
                builder = builder.header("x-caspar-cluster-token", &token);
            }
            let resp = builder
                .send()
                .await
                .map_err(|e| anyhow!("forward: {}", e))?;
            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(anyhow!("leader rejected proposal: {}", text));
            }
            Ok(())
        })
    }

    /// Latency-sorted view of the known peers (nearest first) — the edge
    /// routing table exposed at `/cluster/nearest`.
    pub fn nearest_peers(&self) -> Vec<PeerHealth> {
        let mut peers: Vec<PeerHealth> = self.rtt.read().unwrap().values().cloned().collect();
        peers.sort_by(|a, b| {
            let ka = (!a.reachable, a.rtt_ms.unwrap_or(f64::MAX));
            let kb = (!b.reachable, b.rtt_ms.unwrap_or(f64::MAX));
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        });
        peers
    }

    pub fn status_json(&self) -> Value {
        let metrics = self.raft.metrics().borrow().clone();
        let cfg = self.config_snapshot();
        json!({
            "nodeId": self.node_id,
            "nodeName": cfg.node_name,
            "region": cfg.region,
            "advertiseAddr": cfg.advertise_addr,
            "state": format!("{:?}", metrics.state),
            "currentLeader": metrics.current_leader,
            "term": metrics.current_term,
            "lastLogIndex": metrics.last_log_index,
            "lastApplied": metrics.last_applied,
            "membership": metrics.membership_config,
            "replication": metrics.replication,
            "droppedProposals": self.dropped_proposals.load(Ordering::Relaxed),
            "peers": self.nearest_peers(),
        })
    }

    // ── background workers ────────────────────────────────────────────────

    fn spawn_replication_worker(self: &Arc<Self>, rx: crossbeam_channel::Receiver<ClusterCommand>) {
        let svc = self.clone();
        std::thread::Builder::new()
            .name("cluster-replicator".into())
            .spawn(move || {
                while let Ok(cmd) = rx.recv() {
                    let mut delay = Duration::from_millis(200);
                    // Retry with backoff: leadership may be mid-election. A
                    // command is dropped only after ~30s of failed attempts.
                    let deadline = Instant::now() + Duration::from_secs(30);
                    loop {
                        match svc.propose_blocking(&cmd) {
                            Ok(()) => break,
                            Err(e) => {
                                if Instant::now() >= deadline {
                                    eprintln!("[cluster] proposal dropped: {}", e);
                                    break;
                                }
                                std::thread::sleep(delay);
                                delay = (delay * 2).min(Duration::from_secs(5));
                            }
                        }
                    }
                }
            })
            .expect("spawn cluster-replicator");
    }

    fn spawn_rtt_prober(self: &Arc<Self>) {
        let svc = self.clone();
        std::thread::Builder::new()
            .name("cluster-rtt-prober".into())
            .spawn(move || loop {
                let cfg = svc.config_snapshot();
                let mut targets: BTreeMap<u64, PeerConfig> = cfg.peers.clone();
                // Include membership nodes the config may not know yet.
                let metrics = svc.raft.metrics().borrow().clone();
                for (id, node) in metrics.membership_config.membership().nodes() {
                    targets.entry(*id).or_insert_with(|| PeerConfig {
                        id: *id,
                        addr: node.addr.clone(),
                        ..Default::default()
                    });
                }
                targets.remove(&svc.node_id);
                for (id, peer) in targets {
                    let started = Instant::now();
                    let ok = svc.probe_peer(&peer.addr);
                    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
                    let mut table = svc.rtt.write().unwrap();
                    let entry = table.entry(id).or_insert_with(|| PeerHealth {
                        node_id: id,
                        addr: peer.addr.clone(),
                        region: peer.region.clone(),
                        rtt_ms: None,
                        reachable: false,
                    });
                    entry.addr = peer.addr.clone();
                    if !peer.region.is_empty() {
                        entry.region = peer.region.clone();
                    }
                    entry.reachable = ok;
                    if ok {
                        entry.rtt_ms = Some(match entry.rtt_ms {
                            Some(prev) => prev * 0.7 + elapsed_ms * 0.3,
                            None => elapsed_ms,
                        });
                    }
                }
                std::thread::sleep(Duration::from_secs(cfg.rtt_probe_interval_secs.max(3)));
            })
            .expect("spawn cluster-rtt-prober");
    }

    fn probe_peer(&self, addr: &str) -> bool {
        let token = self.auth_token();
        let url = format!("http://{}/cluster/ping", addr);
        self.rt.block_on(async {
            let client = reqwest::Client::new();
            let mut builder = client.get(&url).timeout(Duration::from_secs(3));
            if !token.is_empty() {
                builder = builder.header("x-caspar-cluster-token", &token);
            }
            matches!(builder.send().await, Ok(resp) if resp.status().is_success())
        })
    }
}

// ────────────────────────── Command application ────────────────────────────

/// Applies committed raft commands to this node's state.
struct NodeApplier {
    app: Arc<dyn ICore>,
    node_id: u64,
}

impl CommandApplier for NodeApplier {
    fn apply(&self, cmd: &ClusterCommand) -> command::ClusterResponse {
        match cmd {
            ClusterCommand::Noop | ClusterCommand::ConfigPut { .. } => {
                command::ClusterResponse::ok()
            }
            ClusterCommand::KvBatch { origin, ops } => {
                if *origin == self.node_id {
                    // Already applied locally before it was proposed.
                    return command::ClusterResponse::ok();
                }
                apply_kv_batch(&self.app, ops)
            }
            ClusterCommand::Deploy { origin, artifact } => {
                if *origin == self.node_id {
                    return command::ClusterResponse::ok();
                }
                match apply_deploy_artifact(&self.app, artifact) {
                    Ok(()) => command::ClusterResponse::ok(),
                    Err(e) => command::ClusterResponse::err(e.to_string()),
                }
            }
        }
    }
}

fn apply_kv_batch(app: &Arc<dyn ICore>, ops: &[KvOp]) -> command::ClusterResponse {
    let db = app.tools().storage().kv_db();
    let batch = ops
        .iter()
        .map(|op| match op {
            KvOp::Put { key, .. } => aseman_storage_legacy::LegacyKvWrite::Put {
                key: key.as_bytes().to_vec(),
                value: op.decoded_value(),
            },
            KvOp::Del { key } => aseman_storage_legacy::LegacyKvWrite::Delete {
                key: key.as_bytes().to_vec(),
            },
        })
        .collect::<Vec<_>>();
    match db.write_batch(&batch) {
        Ok(()) => command::ClusterResponse::ok(),
        Err(e) => command::ClusterResponse::err(format!("kv batch: {}", e)),
    }
}

/// Materialize a distributed creature deployment on this instance: persist
/// the artifact files, restore the program/entity records and runtime links,
/// and register the VMM listener — the same effects `/programs/deploy` had
/// on the origin instance.
fn apply_deploy_artifact(app: &Arc<dyn ICore>, artifact: &DeployArtifact) -> Result<()> {
    let build_folder_path = format!(
        "{}/machines/{}/entities/{}",
        app.tools().storage().storage_root(),
        artifact.program_id,
        artifact.entity_id
    );
    for (name, data_b64) in &artifact.files {
        let data = B64
            .decode(data_b64)
            .map_err(|e| anyhow!("artifact file {}: {}", name, e))?;
        app.tools()
            .file()
            .save_data_to_global_storage(&build_folder_path, &data, name, true)?;
    }

    let artifact_owned = artifact.clone();
    let folder = build_folder_path.clone();
    with_applying_guard(|| {
        app.modify_state(
            false,
            Box::new(move |trx: &dyn ITrx| {
                let art = &artifact_owned;
                if art.set_entity_links {
                    trx.put_link(
                        &format!("vmEntityPath::{}::{}", art.program_id, art.entity_id),
                        &format!("{}/{}", folder, art.primary_file_name),
                    );
                    trx.put_link(
                        &format!("vmEntityType::{}::{}", art.program_id, art.entity_id),
                        &art.entity_type,
                    );
                }
                trx.put_link(&format!("vmDistribution::{}", art.program_id), "cluster");
                trx.put_link(
                    &format!("vmDistribution::{}::{}", art.program_id, art.entity_id),
                    "cluster",
                );
                // Mirror the origin's custom VM gateway route (reconciling any
                // stale prior route) so the friendly path resolves on every
                // instance the creature was propagated to.
                crate::shell::api::actions::program::register_gateway_route(
                    trx,
                    &art.machine_id,
                    &art.program_id,
                    &art.entity_id,
                    &art.gateway_route,
                    &art.gateway_vm_id,
                    &art.entity_type,
                )?;
                // LD-17: the propagated program goes through the directory, so its
                // `machinePrograms` link is written with it.
                let programs = crate::shell::api::model::program_ports::ProgramPorts { trx };
                let record = aseman_domain::program::ProgramRecord {
                    id: art.program_id.clone(),
                    machine_id: art.machine_id.clone(),
                    runtime: art.runtime.clone(),
                    path: art.path.clone(),
                    comment: art.comment.clone(),
                };
                match aseman_ports::ProgramDirectory::update_program(&programs, &record) {
                    Err(aseman_ports::PortError::NotFound) => {
                        aseman_ports::ProgramDirectory::create_program(&programs, &record)
                    }
                    other => other,
                }
                .map_err(|error| anyhow!("{error}"))?;
                crate::shell::api::model::Entity {
                    program_id: art.program_id.clone(),
                    entity_id: art.entity_id.clone(),
                    entity_type: art.entity_type.clone(),
                    image_name: art.entity_id.clone(),
                }
                .push(trx);
                Ok(())
            }),
        );
    });

    app.tools().vmm().assign(&artifact.program_id);
    if artifact.build_on_deploy {
        let app_async = app.clone();
        let mid = artifact.program_id.clone();
        let eid = artifact.entity_id.clone();
        let etype = artifact.entity_type.clone();
        std::thread::spawn(move || {
            app_async
                .tools()
                .vmm()
                .build_vm_image(&mid, &eid, &build_folder_path, &etype);
        });
    }
    Ok(())
}

// ────────────────────────── Bootstrap ───────────────────────────────────────

/// Start the cluster subsystem when enabled by config/env. Called once from
/// `main` after the core is loaded; a standalone node (the default) returns
/// immediately and behaves exactly as before.
pub fn init(app: Arc<dyn ICore>, source: &ClusterBootstrapConfig) {
    let storage_root = app.tools().storage().storage_root();
    let (cfg, path) = ClusterConfig::bootstrap(&storage_root, source);
    if !cfg.enabled {
        if cfg.auth_token.is_empty() {
            return;
        }
        let module_root = PathBuf::from(&storage_root).join("modules");
        let admin: Arc<dyn ModuleAdministration> = match ModuleAdminService::open(module_root) {
            Ok(admin) => Arc::new(admin),
            Err(error) => {
                eprintln!("[modules] failed to initialize administration: {error}");
                return;
            }
        };
        if let Err(error) =
            server::start_module_admin(admin, cfg.listen_addr.clone(), cfg.auth_token.clone())
        {
            eprintln!("[modules] failed to start administration: {error}");
        } else {
            eprintln!(
                "[modules] authenticated administration listener on {}",
                cfg.listen_addr
            );
        }
        return;
    }
    match start(app, cfg, path) {
        Ok(svc) => {
            let listen = svc.config_snapshot().listen_addr;
            eprintln!(
                "[cluster] node {} joined the mesh — raft/admin listener on {}",
                svc.node_id, listen
            );
        }
        Err(e) => {
            eprintln!("[cluster] failed to start cluster subsystem: {}", e);
        }
    }
}

fn start(
    app: Arc<dyn ICore>,
    cfg: ClusterConfig,
    config_path: PathBuf,
) -> Result<Arc<ClusterService>> {
    let svc = start_service(app, cfg, config_path)?;
    CLUSTER
        .set(svc.clone())
        .map_err(|_| anyhow!("cluster already initialized"))?;
    Ok(svc)
}

/// Build and boot a [`ClusterService`] (raft, HTTP listener, replication
/// worker, RTT prober) without registering it as the process-global
/// instance. `start` wraps this; tests use it directly to run several
/// instances inside one process.
pub(crate) fn start_service(
    app: Arc<dyn ICore>,
    cfg: ClusterConfig,
    config_path: PathBuf,
) -> Result<Arc<ClusterService>> {
    let module_root = PathBuf::from(app.tools().storage().storage_root()).join("modules");
    let module_admin: Arc<dyn ModuleAdministration> =
        Arc::new(ModuleAdminService::open(module_root)?);
    let raft_dir = config_path
        .parent()
        .map(|p| p.join("raft-db"))
        .unwrap_or_else(|| PathBuf::from("raft-db"));
    let db = store::open_db(&raft_dir)?;
    let log_store = LogStore::new(db.clone());
    let applier: Arc<dyn CommandApplier> = Arc::new(NodeApplier {
        app: app.clone(),
        node_id: cfg.node_id,
    });
    let sm =
        StateMachineStore::new(db, applier).map_err(|e| anyhow!("state machine open: {}", e))?;
    let raft_config = Arc::new(
        cfg.raft_config()
            .validate()
            .map_err(|e| anyhow!("raft config: {}", e))?,
    );
    let network = network::HttpNetworkFactory {
        auth_token: cfg.auth_token.clone(),
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("cluster-raft")
        .enable_all()
        .build()?;
    let raft = rt
        .block_on(Raft::<TypeConfig>::new(
            cfg.node_id,
            raft_config,
            network,
            log_store,
            sm,
        ))
        .map_err(|e| anyhow!("raft start: {}", e))?;

    // Seed auto-init: only the instance marked `bootstrap: true` initializes
    // a brand-new raft with itself, becoming the first voter that accepts
    // the other instances via add-peer. Joining instances must stay
    // pristine — if they self-initialized they could never be merged into
    // the seed's cluster (two independent single-node leaders).
    if cfg.bootstrap {
        let is_initialized = rt.block_on(raft.is_initialized()).unwrap_or(false);
        if !is_initialized {
            let mut members = BTreeMap::new();
            members.insert(cfg.node_id, BasicNode::new(cfg.advertise_addr.clone()));
            let init_res = rt.block_on(raft.initialize(members));
            if let Err(e) = init_res {
                // A raft that has voted before refuses re-init; that's fine.
                eprintln!("[cluster] initialize skipped: {}", e);
            }
        }
    }

    let (proposal_tx, proposal_rx) =
        crossbeam_channel::bounded::<ClusterCommand>(cfg.proposal_queue_capacity.max(16));

    let svc = Arc::new(ClusterService {
        node_id: cfg.node_id,
        app,
        raft,
        rt,
        config: RwLock::new(cfg),
        config_path,
        proposal_tx,
        rtt: RwLock::new(HashMap::new()),
        dropped_proposals: AtomicU64::new(0),
        module_admin,
    });

    svc.spawn_replication_worker(proposal_rx);
    svc.spawn_rtt_prober();
    server::start(svc.clone())?;
    Ok(svc)
}

#[cfg(test)]
mod tests;
