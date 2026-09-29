//! The chain driver over Babble. Each work chain owns a main shard (`shard-main`)
//! and possibly more; each shard is a Babble engine and its proxy. Requests the
//! node submits are framed onto a shard, and committed blocks flow back through
//! the node's pipeline.

use aseman_fs::{Access, write_atomic};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Result;
use dashmap::DashMap;

use crate::node::Node;
use crate::state::{Chain, ChainShard};
use crate::storage::Trx;
use crate::transports::chain::callbacks::PipelineFn;
use crate::transports::chain::globe::ChainPacketOp;
use aseman_consensus_hashgraph::babble::{Babble, load_key_for_config};
use aseman_consensus_hashgraph::config::Config;
use aseman_consensus_hashgraph::hashgraph::{Block, FrameLimits, InternalTransactionReceipt};
use aseman_consensus_hashgraph::net::Transport;
use aseman_consensus_hashgraph::node::state::State as NodeState;
use aseman_consensus_hashgraph::peers::{Peer, PeerSet};
use aseman_consensus_hashgraph::proxy::{CommitResponse, InmemProxy, ProxyHandler};
use aseman_network_shell::TlsConfig;

use super::shard_bootstrap::{self, Bootstrap, PeerMode};

/// Extract the host portion of a peer's `net_addr` (`host:port` → `host`).
fn peer_host(net_addr: &str) -> String {
    net_addr.split(':').next().unwrap_or(net_addr).to_string()
}

/// Submission envelope routed onto the chain dispatch channel (chain-module-owned).
#[derive(Clone)]
pub(crate) struct ChainSubmission {
    pub(crate) chain_id: String,
    pub(crate) op: crate::transports::chain::globe::ChainPacketOp,
}

/// A work chain: one main shard + many sub-shards.
struct WorkChain {
    id: String,
    blockchain: std::sync::Weak<Blockchain>,
    main_ledger: Mutex<Option<Arc<Mutex<Babble>>>>,
    main_proxy: Mutex<Option<Arc<InmemProxy>>>,
    shard_chains: DashMap<String, Arc<ShardChain>>,
}

/// A single shard.
struct ShardChain {
    shard_ledger: Arc<Mutex<Babble>>,
    shard_proxy: Arc<InmemProxy>,
    /// Live, dynamically-updated set of the shard's consensus peer hosts.
    ///
    /// This is the source `Blockchain::peers()` reads — it MUST NOT re-lock
    /// `shard_ledger` or the consensus `core` from there. `peers()` runs inside
    /// the block-commit handler, which executes while:
    ///   * the consensus run loop holds the `shard_ledger` mutex for the
    ///     engine's whole lifetime (`engine.run()` blocks until shutdown), and
    ///   * `Core::insert_event_and_run_consensus` holds the `core` mutex.
    ///     Re-locking either from `peers()` self-deadlocks the node (and, transi-
    ///     tively, every thread needing consensus / the core / the TCP API).
    ///
    /// The consensus `Core` updates this cache on every `set_peers`, so it
    /// tracks dynamic membership changes (joins/leaves) rather than freezing at
    /// the init snapshot, while staying readable under its own independent lock.
    peer_hosts: Arc<Mutex<Vec<String>>>,
}

/// The consensus log of one shard chain: `chains/{work_chain}/{shard}`.
pub(crate) fn consensus_log_name(work_chain: &str, shard: &str) -> String {
    format!("chains/{work_chain}/{shard}")
}

/// Where shard bootstrap keeps key and peer files when none is configured.
const DEFAULT_BABBLE_DATA_DIR: &str = "/root/.babble";

/// The settings every shard engine of this node runs with.
#[derive(Clone, Debug)]
pub struct ChainSettings {
    /// This node heads the main chain (bootstraps it rather than joining).
    pub is_head: bool,
    /// Where the validator key pair and peer files are kept for shard bootstrap;
    /// a generated key is mirrored there only when it is configured.
    pub babble_data_dir: Option<String>,
    /// The node a joining shard fetches its peer set from.
    pub root_node: Option<String>,
    /// The consensus API port, and the address it is advertised on.
    pub api_port: u16,
    pub ip_address: String,
    pub frame_limits: FrameLimits,
}

impl Default for ChainSettings {
    fn default() -> Self {
        Self {
            is_head: false,
            babble_data_dir: None,
            root_node: None,
            api_port: 1337,
            ip_address: String::new(),
            frame_limits: FrameLimits::default(),
        }
    }
}

impl ChainSettings {
    #[must_use]
    pub fn from_config(config: &aseman_config::AsemanConfig) -> Self {
        let services = &config.services;
        let defaults = Self::default();
        Self {
            is_head: services.is_head,
            babble_data_dir: services.babble_data_dir.clone(),
            root_node: config.core.root_node.clone(),
            api_port: services.blockchain_api_port,
            ip_address: services.ip_address.clone(),
            frame_limits: FrameLimits {
                cached: positive_or(services.babble_frame_cache, defaults.frame_limits.cached),
                retained_rounds: positive_or(
                    services.babble_frame_retention,
                    defaults.frame_limits.retained_rounds,
                ),
            },
        }
    }
}

/// `value` when positive, else `default`.
fn positive_or<T: Default + PartialOrd>(value: T, default: T) -> T {
    if value > T::default() { value } else { default }
}

/// Top-level blockchain driver.
pub struct Blockchain {
    app: Arc<Node>,
    /// Wrapped in Arc so self_clone() shares the same map (DashMap::clone is a deep copy).
    chains: Arc<DashMap<String, Arc<WorkChain>>>,
    pipeline: Mutex<Option<Arc<PipelineFn>>>,
    trans: Mutex<Option<Arc<dyn Transport>>>,
    storage_root: String,
    settings: ChainSettings,
    /// Outbound submission queue: chain packets are framed and pushed onto a
    /// shard engine by the internal drain thread (owned by the chain module).
    chain_tx: crossbeam_channel::Sender<ChainSubmission>,
    /// The consensus provider whose application proxy owns governance
    /// (staking/election) and finance on the main chain. Request/response/message
    /// transactions are forwarded to the registered pipeline; everything else is
    /// consumed by the provider.
    consensus: Option<Arc<aseman_consensus_hashgraph::provider::HashgraphConsensusProvider>>,
    /// Where every shard engine keeps its persistent log: the selected storage
    /// provider's consensus-log storage (ADR 0035).
    log_storage: Option<Arc<dyn aseman_ports::consensus_log::ConsensusLogStorage>>,
    /// Chain base-request response callbacks (owned by the chain module).
    callbacks: Mutex<HashMap<String, Arc<crate::transports::chain::callbacks::ChainCallback>>>,
    /// Typed-message reply callbacks (owned by the chain module).
    // Weak handle back to the original Arc<Blockchain>. WorkChains downgrade
    // from this rather than from the short-lived shim Arc produced by
    // self_clone(), which would otherwise be dropped immediately and leave
    // WorkChain.blockchain.upgrade() returning None during commit_handler.
    weak_self: std::sync::Weak<Blockchain>,
}

impl Blockchain {
    /// A chain with no consensus provider or log storage (tests).
    #[cfg(test)]
    pub fn new(app: Arc<Node>, storage_root: &str) -> Arc<Blockchain> {
        Self::build(app, storage_root, ChainSettings::default(), None, None)
    }

    /// A chain with the consensus provider installed as the main
    /// chain's application handler.
    pub fn with_consensus(
        app: Arc<Node>,
        storage_root: &str,
        settings: ChainSettings,
        consensus: Option<Arc<aseman_consensus_hashgraph::provider::HashgraphConsensusProvider>>,
        log_storage: Arc<dyn aseman_ports::consensus_log::ConsensusLogStorage>,
    ) -> Arc<Blockchain> {
        Self::build(app, storage_root, settings, consensus, Some(log_storage))
    }

    fn build(
        app: Arc<Node>,
        storage_root: &str,
        settings: ChainSettings,
        consensus: Option<Arc<aseman_consensus_hashgraph::provider::HashgraphConsensusProvider>>,
        log_storage: Option<Arc<dyn aseman_ports::consensus_log::ConsensusLogStorage>>,
    ) -> Arc<Blockchain> {
        let storage_root = storage_root.to_string();
        let (chain_tx, chain_rx) = crossbeam_channel::unbounded::<ChainSubmission>();
        let chain = Arc::new_cyclic(|weak| Blockchain {
            app,
            chains: Arc::new(DashMap::new()),
            pipeline: Mutex::new(None),
            trans: Mutex::new(None),
            storage_root,
            settings,
            chain_tx,
            consensus,
            log_storage,
            callbacks: Mutex::new(HashMap::new()),
            weak_self: weak.clone(),
        });
        chain.spawn_submission_drain(chain_rx);
        chain
    }

    /// The chain module's own submission drain: frame each queued packet as
    /// `typ::payload` and push it onto the target shard engine.
    fn spawn_submission_drain(&self, chain_rx: crossbeam_channel::Receiver<ChainSubmission>) {
        let me = self_clone(self);
        std::thread::spawn(move || {
            while let Ok(envelope) = chain_rx.recv() {
                let chain_id = if envelope.chain_id.is_empty() {
                    "main".to_string()
                } else {
                    envelope.chain_id.clone()
                };
                let (typ, payload) = match &envelope.op {
                    ChainPacketOp::BaseRequest(req) => (
                        "base".to_string(),
                        serde_json::to_vec(req).unwrap_or_default(),
                    ),
                    ChainPacketOp::Message(m) => (
                        "message".to_string(),
                        serde_json::to_vec(m).unwrap_or_default(),
                    ),
                };
                let machine_id = match &envelope.op {
                    ChainPacketOp::Message(m) => me
                        .chain_message_machine_ids(m)
                        .into_keys()
                        .next()
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                let mut framed = Vec::new();
                framed.extend_from_slice(typ.as_bytes());
                framed.extend_from_slice(b"::");
                framed.extend_from_slice(&payload);
                me.submit_trx(&chain_id, &machine_id, &typ, framed);
            }
        });
    }

    /// The machine ids a chain message targets on this node.
    fn chain_message_machine_ids(
        &self,
        packet: &aseman_contracts::wire::chain::ChainMessage,
    ) -> HashMap<String, bool> {
        let mut machine_ids = HashMap::new();
        if let Some(map) = packet.recievers.get(&self.app.id()) {
            for key in map.keys() {
                machine_ids.insert(key.clone(), true);
            }
        }
        if let Some(pay) = &packet.pay {
            for machine_id in &pay.machine_ids {
                machine_ids.insert(machine_id.clone(), true);
            }
        }
        machine_ids
    }

    fn create_new_work_chain(
        self: &Arc<Self>,
        chain_id: &str,
        store_id: &str,
        persist: bool,
    ) -> Arc<WorkChain> {
        if let Some(existing) = self.chains.get(chain_id) {
            return existing.value().clone();
        }
        let wchain = Arc::new(WorkChain {
            id: chain_id.to_string(),
            blockchain: self.weak_self.clone(),
            main_ledger: Mutex::new(None),
            main_proxy: Mutex::new(None),
            shard_chains: DashMap::new(),
        });
        self.chains.insert(chain_id.to_string(), wchain.clone());
        // Create the canonical main shard.
        let main_shard = self.create_new_shard_chain(&wchain, "shard-main", false, &[], persist);
        *wchain.main_ledger.lock().unwrap() = Some(main_shard.shard_ledger.clone());
        *wchain.main_proxy.lock().unwrap() = Some(main_shard.shard_proxy.clone());
        if persist {
            let chain_id_owned = chain_id.to_string();
            let store_id_owned = store_id.to_string();
            if let Err(error) = self.app.in_action(|trx: &Trx| {
                Chain {
                    id: chain_id_owned.clone(),
                    store_id: store_id_owned.clone(),
                }
                .save(trx)
            }) {
                eprintln!("storage: {error}");
            }
        }
        wchain
    }

    fn create_new_shard_chain(
        self: &Arc<Self>,
        wchain: &Arc<WorkChain>,
        chain_id: &str,
        created: bool,
        peers_arr: &[String],
        persist: bool,
    ) -> Arc<ShardChain> {
        if let Some(existing) = wchain.shard_chains.get(chain_id) {
            return existing.value().clone();
        }
        // The main chain's application handler is the consensus
        // provider's proxy, which owns staking/election and finance from
        // committed blocks and forwards only request/response/message to the
        // registered pipeline. Sub-shards keep the HgHandler.
        let proxy = if wchain.id == "main" && chain_id == "shard-main" {
            match self.consensus.as_ref() {
                Some(provider) => provider.proxy(),
                None => {
                    let handler: Arc<dyn ProxyHandler> = Arc::new(HgHandler {
                        chain: wchain.clone(),
                        state: Mutex::new(NodeState::default()),
                    });
                    Arc::new(InmemProxy::new(handler, None))
                }
            }
        } else {
            let handler: Arc<dyn ProxyHandler> = Arc::new(HgHandler {
                chain: wchain.clone(),
                state: Mutex::new(NodeState::default()),
            });
            Arc::new(InmemProxy::new(handler, None))
        };

        let data_dir = format!("{}/chains/{}/{}", self.storage_root, wchain.id, chain_id);
        let _ = fs::create_dir_all(&data_dir);

        let peer_mode = if created {
            // Filter the main-chain peer set down to the requested peers.
            if let Some(main_chain) = self.chains.get("main") {
                let main_chain = main_chain.value().clone();
                let main_ledger = main_chain.main_ledger.lock().unwrap().clone();
                if let Some(ledger) = main_ledger {
                    let babble = ledger.lock().unwrap();
                    if let Some(p) = &babble.peers {
                        let peers_list: Vec<&Peer> = p
                            .peers
                            .iter()
                            .filter(|peer| {
                                let host = peer
                                    .net_addr
                                    .split(':')
                                    .next()
                                    .unwrap_or(&peer.net_addr)
                                    .to_string();
                                peers_arr.contains(&host)
                            })
                            .collect();
                        let peerset = PeerSet::new(peers_list.into_iter().cloned().collect());
                        let path = Path::new(&data_dir).join("peers.json");
                        let written = peerset.marshal().and_then(|json| {
                            write_atomic(&path, &json, Access::Shared).map_err(Into::into)
                        });
                        if let Err(error) = written {
                            eprintln!("cannot write the peer set {}: {error}", path.display());
                        }
                    }
                }
            }
            PeerMode::NewShard
        } else if self.settings.is_head {
            PeerMode::Head
        } else {
            PeerMode::Follower
        };

        let babble_data_dir = self
            .settings
            .babble_data_dir
            .as_deref()
            .unwrap_or(DEFAULT_BABBLE_DATA_DIR);
        if let Err(error) = shard_bootstrap::bootstrap(&Bootstrap {
            storage_root: Path::new(&self.storage_root),
            babble_data_dir: Path::new(babble_data_dir),
            workchain_id: &wchain.id,
            shardchain_id: chain_id,
            peer_mode,
            root_node: self.settings.root_node.as_deref(),
        }) {
            eprintln!(
                "FATAL: consensus shard bootstrap failed for {}/{}: {}",
                wchain.id, chain_id, error
            );
            std::process::exit(1);
        }

        let settings = &self.settings;
        let mut config =
            Config::new_default_config(&format!("{}:{}", settings.ip_address, settings.api_port));
        config.bind_addr = format!("0.0.0.0:{}", settings.api_port);
        config.set_data_dir(&data_dir);
        config.frame_limits = settings.frame_limits;
        config.key_mirror_dir = settings.babble_data_dir.clone();
        // The consensus log's name is relative, so it stays the same on any storage
        // provider and under any storage root (ADR 0036).
        config.database_dir = consensus_log_name(&wchain.id, chain_id);
        config.proxy = Some(proxy.clone());
        config.log_storage = self.log_storage.clone();
        // Load the validator key so Babble can sign events.
        if let Err(e) = load_key_for_config(&mut config) {
            eprintln!("load chain key for {}/{}: {}", wchain.id, chain_id, e);
        }
        let arc_config = Arc::new(config);
        let mut engine = Babble::new(arc_config);
        let transport = self.trans.lock().unwrap().clone();
        if let Err(e) = engine.init(transport, &wchain.id, chain_id, None) {
            // A consensus engine that fails to init (e.g. babble's RocksDB store
            // can't be created on a full disk) is fatal: the node would keep
            // serving reads/signals while every consensus write — creature
            // deploy, metering — hangs forever, a silent brick that no health
            // check catches. Exit instead so the supervisor restarts
            // the node; once the underlying cause (disk) clears, the store init
            // succeeds on the next start.
            eprintln!(
                "FATAL: babble consensus init failed for {}/{}: {} — exiting so the node is restarted rather than run with dead consensus",
                wchain.id, chain_id, e
            );
            std::process::exit(1);
        }
        // Independent, lock-free-of-the-engine peer-host cache. Seed it with the
        // engine's current peer set and hand a clone to the consensus core so
        // every `set_peers` keeps it current (dynamic membership). `peers()`
        // reads this cache instead of locking `shard_ledger`/`core`, which the
        // commit handler already holds — see `ShardChain.peer_hosts`. Installed
        // here, while we still own `engine` exclusively (before `run()`), so the
        // brief `core` lock inside `attach_peer_cache` never contends.
        let peer_hosts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(
            engine
                .peers
                .as_ref()
                .map(|ps| ps.peers.iter().map(|p| peer_host(&p.net_addr)).collect())
                .unwrap_or_default(),
        ));
        engine.attach_peer_cache(peer_hosts.clone());
        let engine = Arc::new(Mutex::new(engine));
        let shard_chain = Arc::new(ShardChain {
            shard_ledger: engine.clone(),
            shard_proxy: proxy.clone(),
            peer_hosts,
        });
        wchain
            .shard_chains
            .insert(chain_id.to_string(), shard_chain.clone());
        if persist {
            let chain_id_owned = chain_id.to_string();
            let work_chain_id_owned = wchain.id.clone();
            if let Err(error) = self.app.in_action(|trx: &Trx| {
                ChainShard {
                    id: chain_id_owned.clone(),
                    work_chain_id: work_chain_id_owned.clone(),
                }
                .save(trx)
            }) {
                eprintln!("storage: {error}");
            }
        }
        let engine_for_run = engine.clone();
        thread::spawn(move || {
            // Babble owns its own goroutines; `run` blocks until shutdown.
            engine_for_run.lock().unwrap().run();
        });
        shard_chain
    }

    fn restore_chains_from_storage_inner(self: &Arc<Self>) {
        let restored_slot = Arc::new(Mutex::new(0u32));
        let (chains, shards) = self
            .app
            .read(|trx| {
                Ok((
                    Chain::all(trx).unwrap_or_default(),
                    ChainShard::all(trx).unwrap_or_default(),
                ))
            })
            .unwrap_or_default();
        let mut shards_by_chain: HashMap<String, Vec<String>> = HashMap::new();
        for s in shards {
            shards_by_chain
                .entry(s.work_chain_id)
                .or_default()
                .push(s.id);
        }
        for c in &chains {
            let wchain = self.create_new_work_chain(&c.id, &c.store_id, false);
            for shard_id in shards_by_chain.get(&c.id).cloned().unwrap_or_default() {
                if shard_id == "shard-main" {
                    continue;
                }
                self.create_new_shard_chain(&wchain, &shard_id, false, &[], false);
            }
            *restored_slot.lock().unwrap() += 1;
        }
        let restored = *restored_slot.lock().unwrap();
        if restored == 0 {
            self.create_new_work_chain("main", "", true);
        }
    }
}

impl Blockchain {
    pub(crate) fn listen(&self, _port: i64, _tls_config: Option<TlsConfig>) {
        // Each shard's engine already runs its own loop on the shared
        // transport. The chain driver doesn't need a dedicated listener —
        // the transport is built once and shared.
    }

    pub(crate) fn restore_from_storage(&self) {
        // We need `Arc<Self>` to drive `restore_chains_from_storage_inner`;
        // the wiring layer that constructs `Blockchain` keeps an `Arc` and
        // calls this method through that. The trait method takes `&self`,
        // so we re-wrap into a temporary Arc that shares the same maps.
        let shim = Arc::new(BlockchainShim {
            inner: self_clone(self),
        });
        shim.inner.restore_chains_from_storage_inner();
    }

    pub(crate) fn submit_trx(
        &self,
        chain_id: &str,
        machine_id: &str,
        _typ: &str,
        payload: Vec<u8>,
    ) {
        let Some(work_chain) = self.chains.get(chain_id) else {
            eprintln!("work chain not found: {}", chain_id);
            return;
        };
        let work_chain = work_chain.value().clone();
        let mut target_shard_id = "shard-main".to_string();
        if !machine_id.is_empty() {
            let subchain = self
                .app
                .read(|trx| {
                    let program = crate::state::program_ports::ProgramPorts { trx }
                        .program_or_empty(machine_id);
                    if program.machine_id.is_empty() {
                        return Ok(None);
                    }
                    let machine = crate::state::creature_ports::CreaturePorts { trx }
                        .creature_or_empty(&program.machine_id);
                    Ok(
                        (machine.chain_id == chain_id && !machine.subchain_id.is_empty())
                            .then_some(machine.subchain_id),
                    )
                })
                .unwrap_or_default();
            if let Some(subchain) = subchain {
                target_shard_id = subchain;
            }
        }
        let Some(target) = work_chain.shard_chains.get(&target_shard_id) else {
            eprintln!(
                "target shard chain not found: {} / {}",
                chain_id, target_shard_id
            );
            return;
        };
        let _ = target.value().shard_proxy.submit_tx(&payload);
    }

    pub(crate) fn submit_chain_op(&self, chain_id: &str, op: ChainPacketOp) {
        let _ = self.chain_tx.send(ChainSubmission {
            chain_id: chain_id.to_string(),
            op,
        });
    }

    pub(crate) fn consensus_provider(
        &self,
    ) -> Option<Arc<dyn aseman_ports::consensus::ConsensusProvider>> {
        self.consensus.as_ref().map(|provider| {
            Arc::clone(provider) as Arc<dyn aseman_ports::consensus::ConsensusProvider>
        })
    }

    pub(crate) fn register_pipeline(&self, pipeline: PipelineFn) {
        let pipeline = Arc::new(pipeline);
        *self.pipeline.lock().unwrap() = Some(Arc::clone(&pipeline));
        // Committed request/response/message transactions reach the
        // node through the consensus provider's forwarder (the provider owns
        // governance and finance internally).
        if let Some(provider) = self.consensus.as_ref() {
            let fwd = Arc::clone(&pipeline);
            provider.set_chain_forwarder(Arc::new(move |txs: Vec<Vec<u8>>| -> Vec<String> {
                let cb: Box<dyn Fn(Vec<u8>) + Send + Sync> = Box::new(|_| {});
                fwd(txs, cb)
            }));
        }
    }
    pub(crate) fn peers(&self) -> Vec<String> {
        let Some(main_chain) = self.chains.get("main") else {
            return Vec::new();
        };
        let Some(main_shard) = main_chain.shard_chains.get("shard-main") else {
            return Vec::new();
        };
        // Read the live peer-host cache, NOT `shard_ledger`/`core`. This runs
        // inside the block-commit handler, which already holds both of those
        // mutexes; re-locking either here self-deadlocks the node. The cache is
        // kept current by the consensus core on every `set_peers`, so it
        // reflects dynamic membership. See `ShardChain.peer_hosts`.
        //
        // Bind to a local so the `MutexGuard` temporary is dropped before the
        // DashMap `Ref`s (`main_chain`/`main_shard`) at end of scope.

        main_shard.peer_hosts.lock().unwrap().clone()
    }
    pub(crate) fn close(&self) {
        for entry in self.chains.iter() {
            for shard in entry.value().shard_chains.iter() {
                let _babble = shard.value().shard_ledger.lock().unwrap();
                // Babble's `Node.leave()` leaves the peer set; the
                // current Rust port doesn't expose `Leave` yet so we drop
                // the lock and rely on the transport's shutdown.
                drop(_babble);
            }
        }
    }

    pub(crate) fn register_chain_callback(
        &self,
        callback_id: &str,
        callback: crate::transports::chain::callbacks::ChainCallback,
    ) {
        self.callbacks
            .lock()
            .unwrap()
            .insert(callback_id.to_string(), Arc::new(callback));
    }

    pub(crate) fn park_chain_callback(&self, callback_id: &str) {
        let mut cbs = self.callbacks.lock().unwrap();
        cbs.entry(callback_id.to_string()).or_insert_with(|| {
            Arc::new(crate::transports::chain::callbacks::ChainCallback {
                fn_: Arc::new(|_, _, _| {}),
            })
        });
    }

    pub(crate) fn take_chain_callback(
        &self,
        callback_id: &str,
    ) -> Option<Arc<crate::transports::chain::callbacks::ChainCallback>> {
        self.callbacks.lock().unwrap().remove(callback_id)
    }
}

/// Internal "fake `Arc<Self>`" used by the chain methods that need an
/// `Arc<Blockchain>` to drive helpers. Cloning the `DashMap` / `Mutex`
/// handles gives a parallel view of the same state; we never persist this
/// shim beyond the immediate call.
fn self_clone(b: &Blockchain) -> Arc<Blockchain> {
    // Prefer the real Arc<Blockchain> if it's still alive — otherwise the
    // WorkChains we create here would downgrade against a throwaway Arc
    // and their Weak<Blockchain> would be dead on arrival.
    if let Some(strong) = b.weak_self.upgrade() {
        return strong;
    }
    Arc::new(Blockchain {
        app: b.app.clone(),
        chains: Arc::clone(&b.chains),
        pipeline: Mutex::new(None),
        trans: Mutex::new(b.trans.lock().unwrap().clone()),
        storage_root: b.storage_root.clone(),
        settings: b.settings.clone(),
        chain_tx: b.chain_tx.clone(),
        consensus: b.consensus.clone(),
        log_storage: b.log_storage.clone(),
        callbacks: Mutex::new(HashMap::new()),
        weak_self: b.weak_self.clone(),
    })
}

struct BlockchainShim {
    inner: Arc<Blockchain>,
}

struct HgHandler {
    chain: Arc<WorkChain>,
    state: Mutex<NodeState>,
}

impl ProxyHandler for HgHandler {
    fn commit_handler(&self, block: Block) -> Result<CommitResponse> {
        let blockchain = self.chain.blockchain.upgrade();
        let txs = block.transactions().to_vec();
        if let Some(bc) = blockchain {
            // Clone the pipeline Arc under the lock, drop the guard,
            // then dispatch. The pipeline closure can be slow (it
            // dispatches per-tx handlers that re-enter modify_state,
            // build wasm entities, etc.) and holding `bc.pipeline`
            // through that window starves any concurrent call site
            // that wants the lock. Cloning the Arc keeps the closure
            // alive for the call without keeping the guard.
            let pipeline_arc = bc.pipeline.lock().unwrap().clone();
            if let Some(pipeline) = pipeline_arc {
                let cb: Box<dyn Fn(Vec<u8>) + Send + Sync> = Box::new(|_| {});
                let _: Vec<String> = pipeline(txs.clone(), cb);
            }
        }
        let receipts: Vec<InternalTransactionReceipt> = block
            .internal_transactions()
            .iter()
            .map(|it| it.as_accepted())
            .collect();
        Ok(CommitResponse {
            state_hash: b"statehash".to_vec(),
            internal_transaction_receipts: receipts,
        })
    }

    fn state_change_handler(&self, state: NodeState) -> Result<()> {
        *self.state.lock().unwrap() = state;
        Ok(())
    }

    fn snapshot_handler(&self, _block_index: i64) -> Result<Vec<u8>> {
        Ok(b"statehash".to_vec())
    }

    fn restore_handler(&self, _snapshot: &[u8]) -> Result<Vec<u8>> {
        Ok(b"statehash".to_vec())
    }
}
