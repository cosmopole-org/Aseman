//! The RocksDB provider's cluster mode (ADR 0033): OpenRaft distributes the database.
//!
//! Replicas of one storage cluster share a Raft log of key/value write batches. A write
//! through [`ReplicatedKvStore`] is proposed to the leader (forwarded when this replica
//! follows), committed on a quorum, applied in log order to every replica's RocksDB,
//! and acknowledged once this replica has applied it, so a writer always reads its own
//! writes. Reads are served from the local replica.
//!
//! Membership, the Raft RPC listener, and the `/cluster/*` administration API that
//! `asemanctl cluster` drives are part of this provider. Nothing above the storage seam
//! proposes to the log: the node and its actions see one logical store.

pub mod command;
pub mod config;
pub mod network;
pub mod server;
pub mod store;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use aseman_config::ClusterBootstrapConfig;
use openraft::{BasicNode, Raft};
use serde_json::{Value, json};

use crate::{
    KvExpectation, LegacyKvStore, LegacyKvWrite, LegacyMigrationError, LegacyMigrationResult,
    RocksDbKvStore,
};
use command::{ClusterCommand, KvExpect, KvOp, TypeConfig};
use config::{ClusterConfig, PeerConfig};
use store::{CommandApplier, LogStore, StateMachineStore};

/// Routes the composing process serves on the cluster listener (for example the node's
/// module administration): `(method, path, body)` to a response, or `None` to fall
/// through to the cluster's own routes.
pub type RouteHandler = Arc<dyn Fn(&str, &str, &[u8]) -> Option<(u16, Vec<u8>)> + Send + Sync>;

/// How long a replica waits for a committed batch to be applied locally.
const APPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// Peer latency book-keeping, reported by `/cluster/status` and `/cluster/nearest`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PeerHealth {
    pub node_id: u64,
    pub addr: String,
    pub region: String,
    /// Exponentially weighted RTT in milliseconds; `None` until the first probe.
    pub rtt_ms: Option<f64>,
    pub reachable: bool,
}

/// One running replica of the storage cluster.
pub struct ClusterService {
    pub node_id: u64,
    raft: Raft<TypeConfig>,
    rt: tokio::runtime::Runtime,
    config: RwLock<ClusterConfig>,
    config_path: PathBuf,
    rtt: RwLock<HashMap<u64, PeerHealth>>,
    routes: Option<RouteHandler>,
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

    pub(crate) fn routes(&self) -> Option<&RouteHandler> {
        self.routes.as_ref()
    }

    /// Persist a config mutation and return the updated copy.
    pub fn update_config(&self, f: impl FnOnce(&mut ClusterConfig)) -> Result<ClusterConfig> {
        let mut cfg = self.config.write().unwrap();
        f(&mut cfg);
        cfg.save(&self.config_path)?;
        Ok(cfg.clone())
    }

    /// Set one dotted config key (`asemanctl cluster config set k v`).
    pub fn set_config_key(&self, key: &str, raw_value: &str) -> Result<ClusterConfig> {
        let mut cfg = self.config.write().unwrap();
        let updated = cfg.set_key(key, raw_value)?;
        *cfg = updated.clone();
        cfg.save(&self.config_path)?;
        Ok(updated)
    }

    /// Commit a command on the cluster and return its log index: directly when this
    /// replica leads, else forwarded to the leader over the cluster API.
    pub fn propose_blocking(&self, cmd: &ClusterCommand) -> Result<u64> {
        let metrics = self.raft.metrics().borrow().clone();
        let leader = metrics.current_leader;
        if leader == Some(self.node_id) {
            let response = self
                .rt
                .block_on(self.raft.client_write(cmd.clone()))
                .map_err(|e| anyhow!("raft client_write: {e}"))?;
            if !response.data.ok {
                return Err(anyhow!(
                    "replicated command failed: {}",
                    response.data.err.unwrap_or_default()
                ));
            }
            return Ok(response.log_id.index);
        }
        let Some(leader_id) = leader else {
            return Err(anyhow!("no raft leader elected yet"));
        };
        let leader_addr = metrics
            .membership_config
            .membership()
            .get_node(&leader_id)
            .map(|n| n.addr.clone())
            .ok_or_else(|| anyhow!("leader {leader_id} has no known address"))?;
        let token = self.auth_token();
        let body = serde_json::to_vec(cmd)?;
        let url = format!("http://{leader_addr}/cluster/propose");
        self.rt.block_on(async {
            let client = reqwest::Client::new();
            let mut builder = client
                .post(&url)
                .timeout(Duration::from_secs(30))
                .header("content-type", "application/json")
                .body(body);
            if !token.is_empty() {
                builder = builder.header("x-aseman-cluster-token", &token);
            }
            let resp = builder.send().await.map_err(|e| anyhow!("forward: {e}"))?;
            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(anyhow!("leader rejected proposal: {text}"));
            }
            let reply: Value = resp
                .json()
                .await
                .map_err(|e| anyhow!("forward reply: {e}"))?;
            reply["index"]
                .as_u64()
                .ok_or_else(|| anyhow!("leader reply carries no log index"))
        })
    }

    /// Block until this replica has applied the log through `index`.
    pub fn wait_applied(&self, index: u64) -> Result<()> {
        self.rt
            .block_on(
                self.raft
                    .wait(Some(APPLY_TIMEOUT))
                    .applied_index_at_least(Some(index), "replicated write"),
            )
            .map(|_| ())
            .map_err(|e| anyhow!("waiting for log index {index}: {e}"))
    }

    /// Latency-sorted view of the known peers (nearest first).
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
            "peers": self.nearest_peers(),
        })
    }

    fn spawn_rtt_prober(self: &Arc<Self>) {
        let svc = self.clone();
        std::thread::Builder::new()
            .name("cluster-rtt-prober".into())
            .spawn(move || {
                loop {
                    let cfg = svc.config_snapshot();
                    let mut targets: BTreeMap<u64, PeerConfig> = cfg.peers.clone();
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
                }
            })
            .expect("spawn cluster-rtt-prober");
    }

    fn probe_peer(&self, addr: &str) -> bool {
        let token = self.auth_token();
        let url = format!("http://{addr}/cluster/ping");
        self.rt.block_on(async {
            let client = reqwest::Client::new();
            let mut builder = client.get(&url).timeout(Duration::from_secs(3));
            if !token.is_empty() {
                builder = builder.header("x-aseman-cluster-token", &token);
            }
            matches!(builder.send().await, Ok(resp) if resp.status().is_success())
        })
    }
}

/// Applies committed batches to this replica's RocksDB, in log order, on every replica
/// (the proposer included).
struct KvApplier {
    store: Arc<RocksDbKvStore>,
}

impl CommandApplier for KvApplier {
    fn apply(&self, cmd: &ClusterCommand) -> command::ClusterResponse {
        match cmd {
            ClusterCommand::Noop | ClusterCommand::ConfigPut { .. } => {
                command::ClusterResponse::ok()
            }
            ClusterCommand::KvBatch { ops, expects, .. } => {
                let batch: Vec<LegacyKvWrite> = ops.iter().map(KvOp::to_write).collect();
                if expects.is_empty() {
                    return match self.store.write_batch(&batch) {
                        Ok(()) => command::ClusterResponse::ok(),
                        Err(e) => command::ClusterResponse::err(format!("kv batch: {e}")),
                    };
                }
                let expected = match expects
                    .iter()
                    .map(KvExpect::to_expectation)
                    .collect::<Option<Vec<_>>>()
                {
                    Some(expected) => expected,
                    None => {
                        return command::ClusterResponse::err("kv batch: malformed precondition");
                    }
                };
                match self.store.write_batch_if(&expected, &batch) {
                    Ok(true) => command::ClusterResponse::ok(),
                    Ok(false) => command::ClusterResponse::err(command::PRECONDITION_FAILED),
                    Err(e) => command::ClusterResponse::err(format!("kv batch: {e}")),
                }
            }
        }
    }
}

impl KvExpect {
    fn from_expectation(expectation: &KvExpectation) -> LegacyMigrationResult<Self> {
        use base64::Engine as _;
        Ok(Self {
            key: String::from_utf8(expectation.key.clone()).map_err(|_| {
                LegacyMigrationError::Storage("replicated keys must be UTF-8".to_owned())
            })?,
            sha256_b64: expectation
                .digest
                .map(|digest| base64::engine::general_purpose::STANDARD.encode(digest)),
        })
    }

    fn to_expectation(&self) -> Option<KvExpectation> {
        use base64::Engine as _;
        let digest = match &self.sha256_b64 {
            None => None,
            Some(encoded) => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .ok()?
                    .try_into()
                    .ok()?,
            ),
        };
        Some(KvExpectation {
            key: self.key.as_bytes().to_vec(),
            digest,
        })
    }
}

impl KvOp {
    fn to_write(&self) -> LegacyKvWrite {
        match self {
            KvOp::Put { key, .. } => LegacyKvWrite::Put {
                key: key.as_bytes().to_vec(),
                value: self.decoded_value(),
            },
            KvOp::Del { key } => LegacyKvWrite::Delete {
                key: key.as_bytes().to_vec(),
            },
        }
    }

    fn from_write(write: &LegacyKvWrite) -> LegacyMigrationResult<KvOp> {
        let text = |key: &[u8]| {
            String::from_utf8(key.to_vec()).map_err(|_| {
                LegacyMigrationError::Storage("replicated keys must be UTF-8".to_owned())
            })
        };
        Ok(match write {
            LegacyKvWrite::Put { key, value } => KvOp::put(text(key)?, value),
            LegacyKvWrite::Delete { key } => KvOp::del(text(key)?),
        })
    }
}

/// The RocksDB provider's store: local on one host, replicated through OpenRaft in
/// cluster mode.
pub struct ReplicatedKvStore {
    local: Arc<RocksDbKvStore>,
    cluster: Option<Arc<ClusterService>>,
}

impl ReplicatedKvStore {
    /// A single-host store.
    pub fn local(local: Arc<RocksDbKvStore>) -> Self {
        Self {
            local,
            cluster: None,
        }
    }

    /// The replica's cluster, if it runs in cluster mode.
    pub fn cluster(&self) -> Option<&Arc<ClusterService>> {
        self.cluster.as_ref()
    }
}

impl LegacyKvStore for ReplicatedKvStore {
    fn get(&self, key: &[u8]) -> LegacyMigrationResult<Option<Vec<u8>>> {
        self.local.get(key)
    }

    fn scan_prefix(&self, prefix: &[u8]) -> LegacyMigrationResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.local.scan_prefix(prefix)
    }

    fn scan_all(&self) -> LegacyMigrationResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.local.scan_all()
    }

    fn write_batch(&self, writes: &[LegacyKvWrite]) -> LegacyMigrationResult<()> {
        let Some(cluster) = &self.cluster else {
            return self.local.write_batch(writes);
        };
        if writes.is_empty() {
            return Ok(());
        }
        let ops = writes
            .iter()
            .map(KvOp::from_write)
            .collect::<LegacyMigrationResult<Vec<_>>>()?;
        let storage = |error: anyhow::Error| LegacyMigrationError::Storage(error.to_string());
        let index = cluster
            .propose_blocking(&ClusterCommand::KvBatch {
                origin: cluster.node_id,
                ops,
                expects: Vec::new(),
            })
            .map_err(storage)?;
        cluster.wait_applied(index).map_err(storage)
    }

    fn write_batch_if(
        &self,
        expected: &[KvExpectation],
        writes: &[LegacyKvWrite],
    ) -> LegacyMigrationResult<bool> {
        let Some(cluster) = &self.cluster else {
            return self.local.write_batch_if(expected, writes);
        };
        // The precondition is checked by the state machine, in log order, on every
        // replica: a write another replica committed first makes this one a no-op
        // everywhere.
        let ops = writes
            .iter()
            .map(KvOp::from_write)
            .collect::<LegacyMigrationResult<Vec<_>>>()?;
        let expects = expected
            .iter()
            .map(KvExpect::from_expectation)
            .collect::<LegacyMigrationResult<Vec<_>>>()?;
        let storage = |error: anyhow::Error| LegacyMigrationError::Storage(error.to_string());
        match cluster.propose_blocking(&ClusterCommand::KvBatch {
            origin: cluster.node_id,
            ops,
            expects,
        }) {
            Ok(index) => cluster.wait_applied(index).map(|()| true).map_err(storage),
            Err(error) if error.to_string().contains(command::PRECONDITION_FAILED) => Ok(false),
            Err(error) => Err(storage(error)),
        }
    }
}

/// Open the provider's store under `storage_root`: embedded RocksDB, replicated when the
/// cluster configuration enables it. With clustering off, injected `routes` are still
/// served on an authenticated listener when an auth token is configured.
pub fn open(
    storage_root: &Path,
    local: Arc<RocksDbKvStore>,
    source: &ClusterBootstrapConfig,
    routes: Option<RouteHandler>,
) -> Result<ReplicatedKvStore> {
    let (cfg, path) = ClusterConfig::bootstrap(&storage_root.to_string_lossy(), source);
    if !cfg.enabled {
        if let Some(routes) = routes
            && !cfg.auth_token.is_empty()
        {
            server::start_route_listener(routes, cfg.listen_addr.clone(), cfg.auth_token.clone())?;
        }
        return Ok(ReplicatedKvStore::local(local));
    }
    let svc = start_service(local.clone(), cfg, path, routes)?;
    eprintln!(
        "[storage] replica {} joined the RocksDB cluster on {}",
        svc.node_id,
        svc.config_snapshot().listen_addr
    );
    Ok(ReplicatedKvStore {
        local,
        cluster: Some(svc),
    })
}

/// Boot one replica: Raft, the cluster listener, and the RTT prober.
pub fn start_service(
    local: Arc<RocksDbKvStore>,
    cfg: ClusterConfig,
    config_path: PathBuf,
    routes: Option<RouteHandler>,
) -> Result<Arc<ClusterService>> {
    let raft_dir = config_path
        .parent()
        .map(|p| p.join("raft-db"))
        .unwrap_or_else(|| PathBuf::from("raft-db"));
    let db = store::open_db(&raft_dir)?;
    let log_store = LogStore::new(db.clone());
    let applier: Arc<dyn CommandApplier> = Arc::new(KvApplier { store: local });
    let sm = StateMachineStore::new(db, applier).map_err(|e| anyhow!("state machine open: {e}"))?;
    let raft_config = Arc::new(
        cfg.raft_config()
            .validate()
            .map_err(|e| anyhow!("raft config: {e}"))?,
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
        .map_err(|e| anyhow!("raft start: {e}"))?;

    // Only the seed (`bootstrap: true`) initializes a brand-new cluster with itself;
    // joiners stay pristine until the seed adds them, or they would form their own.
    if cfg.bootstrap {
        let is_initialized = rt.block_on(raft.is_initialized()).unwrap_or(false);
        if !is_initialized {
            let mut members = BTreeMap::new();
            members.insert(cfg.node_id, BasicNode::new(cfg.advertise_addr.clone()));
            if let Err(e) = rt.block_on(raft.initialize(members)) {
                eprintln!("[storage] raft initialize skipped: {e}");
            }
        }
    }

    let svc = Arc::new(ClusterService {
        node_id: cfg.node_id,
        raft,
        rt,
        config: RwLock::new(cfg),
        config_path,
        rtt: RwLock::new(HashMap::new()),
        routes,
    });
    server::start(svc.clone())?;
    svc.spawn_rtt_prober();
    Ok(svc)
}

#[cfg(test)]
mod tests;
