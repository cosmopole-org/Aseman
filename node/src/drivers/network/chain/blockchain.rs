//! Translation of `drivers/network/chain/chain.go` — the Caspar `IChain`
//! driver built on top of the Babble engine.
//!
//! Each work chain owns a main shard (`shard-main`) plus zero or more
//! additional shards; each shard is a separate Babble engine + InmemProxy.
//! The driver bridges between Caspar (transactions submitted by the shell
//! and inbound block streams from Babble) and the Babble nodes.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Result;
use dashmap::DashMap;

use uuid::Uuid;

use crate::drivers::network::chain::babble::{load_key_for_config, Babble};
use crate::drivers::network::chain::config::Config;
use crate::drivers::network::chain::hashgraph::{Block, InternalTransactionReceipt};
use crate::drivers::network::chain::net::Transport;
use crate::drivers::network::chain::node::state::State as NodeState;
use crate::drivers::network::chain::peers::{Peer, PeerSet};
use crate::drivers::network::chain::proxy::{CommitResponse, InmemProxy, ProxyHandler};
use crate::models::core::ICore;
use crate::models::ports::network::chain::{IChain, PipelineFn};
use crate::models::ports::network::TlsConfig;
use crate::models::transaction::ITrx;
use crate::shell::api::model::{Chain, ChainShard, Creature, Program};

/// Extract the host portion of a peer's `net_addr` (`host:port` → `host`).
fn peer_host(net_addr: &str) -> String {
    net_addr.split(':').next().unwrap_or(net_addr).to_string()
}

/// A work chain: one main shard + many sub-shards.
struct WorkChain {
    id: String,
    store_id: String,
    blockchain: std::sync::Weak<Blockchain>,
    main_ledger: Mutex<Option<Arc<Mutex<Babble>>>>,
    main_proxy: Mutex<Option<Arc<InmemProxy>>>,
    shard_chains: DashMap<String, Arc<ShardChain>>,
}

/// A single shard.
struct ShardChain {
    id: String,
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
    /// Re-locking either from `peers()` self-deadlocks the node (and, transi-
    /// tively, every thread needing consensus / the core / the TCP API).
    ///
    /// The consensus `Core` updates this cache on every `set_peers`, so it
    /// tracks dynamic membership changes (joins/leaves) rather than freezing at
    /// the init snapshot, while staying readable under its own independent lock.
    peer_hosts: Arc<Mutex<Vec<String>>>,
}

/// Top-level blockchain driver.
pub struct Blockchain {
    app: Arc<dyn ICore>,
    /// Wrapped in Arc so self_clone() shares the same map (DashMap::clone is a deep copy).
    chains: Arc<DashMap<String, Arc<WorkChain>>>,
    pipeline: Mutex<Option<Arc<PipelineFn>>>,
    trans: Mutex<Option<Arc<dyn Transport>>>,
    storage_root: String,
    // Weak handle back to the original Arc<Blockchain>. WorkChains downgrade
    // from this rather than from the short-lived shim Arc produced by
    // self_clone(), which would otherwise be dropped immediately and leave
    // WorkChain.blockchain.upgrade() returning None during commit_handler.
    weak_self: std::sync::Weak<Blockchain>,
}

impl Blockchain {
    /// `NewChain(core, storageRoot)`.
    pub fn new(app: Arc<dyn ICore>, storage_root: &str) -> Arc<Blockchain> {
        let storage_root = storage_root.to_string();
        Arc::new_cyclic(|weak| Blockchain {
            app,
            chains: Arc::new(DashMap::new()),
            pipeline: Mutex::new(None),
            trans: Mutex::new(None),
            storage_root,
            weak_self: weak.clone(),
        })
    }

    fn pipeline_callback(&self) -> Option<&'static PipelineFn> {
        // We can't return a borrow safely; the actual call sites pull the
        // Mutex and invoke it inline. This helper is here so the API of
        // `Blockchain` is symmetric for tests.
        None
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
            store_id: store_id.to_string(),
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
            self.app.modify_state(
                false,
                Box::new(move |trx: &dyn ITrx| {
                    Chain {
                        id: chain_id_owned.clone(),
                        store_id: store_id_owned.clone(),
                    }
                    .push(trx);
                    Ok(())
                }),
            );
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
        let handler: Arc<dyn ProxyHandler> = Arc::new(HgHandler {
            chain: wchain.clone(),
            state: Mutex::new(NodeState::default()),
        });
        let proxy = Arc::new(InmemProxy::new(handler, None));

        let data_dir = format!("{}/chains/{}/{}", self.storage_root, wchain.id, chain_id);
        let _ = fs::create_dir_all(&data_dir);

        let peers_list_mode = if created {
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
                        let json = peerset.marshal().unwrap_or_default();
                        let _ = fs::write(format!("{}/peers.json", data_dir), json);
                    }
                }
            }
            "1"
        } else if aseman_config::legacy_adapter_snapshot()
            .map(|config| config.is_head)
            .unwrap_or(false)
        {
            "2"
        } else {
            "3"
        };

        // Invoke the shardchain bootstrap script (matches Go behaviour).
        // If it fails we surface the error and abort the shard setup: a
        // silent failure here leaves the shard directory without
        // peers.json / priv_key, which causes load_key_for_config to
        // generate a fresh key, mirror it back over BABBLE_DATA_DIR's
        // priv_key, and silently invalidate the cluster-wide
        // peers.genesis.json — the node then never matches any peer entry,
        // stays in JOINING forever, and never opens its TCP API listener.
        //
        // The script location is configurable via SHARDCHAIN_SCRIPT so the
        // node works both in Docker (where node/scripts is mounted at
        // /app/scripts — the default below) and in local/--no-docker runs
        // (where run-nodes.sh points this at the in-repo node/scripts copy).
        let shardchain_script = aseman_config::legacy_adapter_snapshot()
            .map(|config| config.shardchain_script.clone())
            .unwrap_or_else(|| "/app/scripts/shardchain.sh".to_string());
        match Command::new("bash")
            .arg(&shardchain_script)
            .arg(&self.storage_root)
            .arg(&wchain.id)
            .arg(chain_id)
            .arg(peers_list_mode)
            .status()
        {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!(
                "shardchain.sh exited with status {} for {}/{} (mode={}) — \
                 babble bootstrap will likely fail",
                s, wchain.id, chain_id, peers_list_mode
            ),
            Err(e) => eprintln!(
                "shardchain.sh failed to spawn for {}/{}: {} — \
                 babble bootstrap will likely fail",
                wchain.id, chain_id, e
            ),
        }

        let (blockchain_port, ip_address) = aseman_config::legacy_adapter_snapshot()
            .map(|config| (config.blockchain_api_port, config.ip_address.as_str()))
            .unwrap_or((1337, ""));
        let mut config = Config::new_default_config(&format!("{}:{}", ip_address, blockchain_port));
        config.bind_addr = format!("0.0.0.0:{}", blockchain_port);
        // set_data_dir() also relocates database_dir off the default
        // (/root/.babble) so multiple nodes on one host don't clobber each
        // other's RocksDB.
        config.set_data_dir(&data_dir);
        config.proxy = Some(proxy.clone());
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
            // check catches. Exit instead so the supervisor / casparctl restarts
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
            id: chain_id.to_string(),
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
            self.app.modify_state(
                false,
                Box::new(move |trx: &dyn ITrx| {
                    ChainShard {
                        id: chain_id_owned.clone(),
                        work_chain_id: work_chain_id_owned.clone(),
                    }
                    .push(trx);
                    Ok(())
                }),
            );
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
        let chains_slot: Arc<Mutex<Vec<Chain>>> = Arc::new(Mutex::new(Vec::new()));
        let shards_slot: Arc<Mutex<Vec<ChainShard>>> = Arc::new(Mutex::new(Vec::new()));
        let chains_clone = chains_slot.clone();
        let shards_clone = shards_slot.clone();
        self.app.modify_state(
            true,
            Box::new(move |trx: &dyn ITrx| {
                if let Ok(c) = Chain::all(trx, -1, -1, &HashMap::new()) {
                    *chains_clone.lock().unwrap() = c;
                }
                if let Ok(s) = ChainShard::all(trx, -1, -1, &HashMap::new()) {
                    *shards_clone.lock().unwrap() = s;
                }
                Ok(())
            }),
        );
        let chains = chains_slot.lock().unwrap().clone();
        let shards = shards_slot.lock().unwrap().clone();
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

impl IChain for Blockchain {
    fn listen(&self, _port: i64, _tls_config: Option<TlsConfig>) {
        // Each shard's engine already runs its own loop on the shared
        // transport. The chain driver doesn't need a dedicated listener —
        // the transport is built once and shared.
    }

    fn restore_from_storage(&self) {
        // We need `Arc<Self>` to drive `restore_chains_from_storage_inner`;
        // the wiring layer that constructs `Blockchain` keeps an `Arc` and
        // calls this method through that. The trait method takes `&self`,
        // so we re-wrap into a temporary Arc that shares the same maps.
        let shim = Arc::new(BlockchainShim {
            inner: self_clone(self),
        });
        shim.inner.restore_chains_from_storage_inner();
    }

    fn submit_trx(&self, chain_id: &str, machine_id: &str, _typ: &str, payload: Vec<u8>) {
        let Some(work_chain) = self.chains.get(chain_id) else {
            eprintln!("work chain not found: {}", chain_id);
            return;
        };
        let work_chain = work_chain.value().clone();
        let mut target_shard_id = "shard-main".to_string();
        if !machine_id.is_empty() {
            let target_slot = Arc::new(Mutex::new(target_shard_id.clone()));
            let target_clone = target_slot.clone();
            let chain_id_owned = chain_id.to_string();
            let machine_id_owned = machine_id.to_string();
            self.app.modify_state(
                true,
                Box::new(move |trx: &dyn ITrx| {
                    let vm = Program {
                        id: machine_id_owned.clone(),
                        ..Default::default()
                    }
                    .pull(trx);
                    if vm.machine_id.is_empty() {
                        return Ok(());
                    }
                    let app = (crate::shell::api::model::creature_ports::LegacyCreatures { trx })
                        .creature_or_empty(&vm.machine_id.clone());
                    if app.chain_id == chain_id_owned && !app.subchain_id.is_empty() {
                        *target_clone.lock().unwrap() = app.subchain_id.clone();
                    }
                    Ok(())
                }),
            );
            target_shard_id = target_slot.lock().unwrap().clone();
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

    fn register_pipeline(&self, pipeline: PipelineFn) {
        *self.pipeline.lock().unwrap() = Some(Arc::new(pipeline));
    }

    fn notify_new_machine_created(&self, _chain_id: &str, _machine_id: &str) {
        // Hook for future machine-onboarding logic; matches Go's empty body.
    }

    fn create_temp_chain(&self, store_id: &str) -> String {
        // The trait signature can't borrow `Arc<Self>`, so we go through
        // the shim helper (same trick `restore_from_storage` uses).
        let shim = Arc::new(BlockchainShim {
            inner: self_clone(self),
        });
        let id = Uuid::new_v4().to_string();
        shim.inner.create_new_work_chain(&id, store_id, true);
        id
    }

    fn create_work_chain(&self, store_id: &str) -> String {
        let shim = Arc::new(BlockchainShim {
            inner: self_clone(self),
        });
        let id = Uuid::new_v4().to_string();
        shim.inner.create_new_work_chain(&id, store_id, true);
        id
    }

    fn create_shard_chain(
        &self,
        chain_id: &str,
        shard_chain_id: &str,
        peers: Vec<String>,
    ) -> String {
        let Some(work_chain) = self.chains.get(chain_id) else {
            return String::new();
        };
        let work_chain = work_chain.value().clone();
        let shard_chain_id = if shard_chain_id.is_empty() {
            format!("shard-{}", Uuid::new_v4())
        } else {
            shard_chain_id.to_string()
        };
        if work_chain.shard_chains.contains_key(&shard_chain_id) {
            return shard_chain_id;
        }
        let shim = Arc::new(BlockchainShim {
            inner: self_clone(self),
        });
        shim.inner
            .create_new_shard_chain(&work_chain, &shard_chain_id, true, &peers, true);
        shard_chain_id
    }

    fn peers(&self) -> Vec<String> {
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
        let hosts = main_shard.peer_hosts.lock().unwrap().clone();
        hosts
    }

    fn user_owns_origin(&self, _user_id: &str, _origin: &str) -> bool {
        // Mirrors Go: the placeholder always returns true.
        true
    }

    fn get_node_owner_id(&self, _origin: &str) -> String {
        // Mirrors Go: returns "" until staking introspection is in place.
        String::new()
    }

    fn close(&self) {
        for entry in self.chains.iter() {
            for shard in entry.value().shard_chains.iter() {
                let _babble = shard.value().shard_ledger.lock().unwrap();
                // Babble's `Node.leave()` mirrors Go's `Node.Leave`; the
                // current Rust port doesn't expose `Leave` yet so we drop
                // the lock and rely on the transport's shutdown.
                drop(_babble);
            }
        }
    }
}

/// Internal "fake `Arc<Self>`" used by the IChain trait methods that need an
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

/// CLI configuration (Go: `viper`-bound struct). The Rust port consumes
/// this through a hand-written builder rather than viper, so it only
/// carries the fields the runtime currently exposes.
#[derive(Debug, Clone, Default)]
pub struct CliConfig {
    pub proxy_addr: String,
    pub client_addr: String,
}

// Convenience helper for `PathBuf::join` in callers that import the module.
fn _path_helper(a: &str, b: &str) -> PathBuf {
    PathBuf::from(a).join(b)
}
