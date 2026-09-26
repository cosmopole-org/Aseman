//! Runtime start phase for the legacy `Core` orchestrator: `run` and the
//! strongly-typed `load_inner` that assembles drivers, tools, the globe, and
//! the chain pipeline.
//!
//! Translation of `core/module/core/core.go`.

use std::sync::Arc;

use anyhow::Result;
use aseman_config::AsemanConfig;
use rsa::RsaPrivateKey;

use crate::adapters::network::Network as NetworkDriver;
use crate::adapters::network::chain::Blockchain;
use crate::adapters::network::federation::FedNet;
use crate::adapters::security::Security;
use crate::adapters::signaler::Signaler;
use crate::adapters::storage::Storage;
use crate::adapters::vmm::NodeWorkloads;
use crate::legacy::globe::{ChainPacketOp, Globe};
use crate::legacy::orchestrator::types::{Core, Tools};
use crate::models::chain::{ChainCallback, MessageCallback};
use crate::models::core::ICore;
use crate::models::ports::{
    INetwork, IRateLimiter, ISecurity, ISignaler, IStorage, ITools, IWorkloads,
};
use aseman_network_legacy::tls_config_from_files;
use aseman_ports::consensus::ConsensusProvider as _ConsensusProvider;

impl Core {
    /// Runtime start phase invoked after load/module initialization.
    pub fn run(self: &Arc<Self>) {}

    /// Strongly-typed `Load`. Run once on startup after the constructor.
    #[allow(clippy::if_same_then_else)] // two legacy chain routes intentionally share a callback
    pub fn load_inner(
        self: &Arc<Self>,
        gods: Vec<String>,
        storage_root: &str,
        base_db_path: &str,
        applet_db_path: &str,
        store_logs_db: &str,
        searcher_db: &str,
    ) -> Result<()> {
        *self.gods.lock().unwrap() = gods;
        let _ = applet_db_path; // currently fed straight into Vmm
        let _ = store_logs_db;
        let _ = searcher_db;

        // Stage 1 of federation must run before the rest so we can pass
        // the same `Arc<FedNet>` into the storage / network drivers.
        let fed: Arc<FedNet> = FedNet::first_stage(self.clone());
        let storage: Arc<dyn IStorage> = Storage::new(
            self.clone(),
            storage_root,
            base_db_path,
            store_logs_db,
            searcher_db,
            self.config
                .as_ref()
                .map(|config| config.legacy_adapters.questdb_port)
                .unwrap_or(8812),
        )?;
        let signaler: Arc<dyn ISignaler> = Signaler::new(self.clone(), fed.clone());
        let security: Arc<dyn ISecurity> = Security::new(self.clone(), storage_root);

        // RL-011: one consensus provider owns finance ordering AND validator
        // governance (staking/election) as the main chain's application
        // handler. It is composed once here — before the chain adapter — so
        // `Blockchain` can install its proxy as the main-chain engine handler.
        let provider = Arc::new(
            aseman_consensus_hashgraph::provider::HashgraphConsensusProvider::for_node(
                &self.id, &self.ip,
            ),
        );
        for (key, value) in aseman_config::consensus_env_properties() {
            if let Err(error) = provider.set(&key, &value) {
                eprintln!("consensus property {key} not applied: {error}");
            }
        }
        // Governance is fully autonomous: outbound election packets go through
        // this provider's own chain edge, and the provider runs the hourly
        // election scheduler on its own clock. The node never drives elections.
        provider.wire_governance_submit();
        provider.spawn_election_scheduler();

        // The provider is owned by the chain module (installed as the main-chain
        // application handler); the core orchestrator reaches it via the chain.
        let chain: Arc<dyn crate::models::ports::IChain> =
            Blockchain::with_consensus(self.clone(), storage_root, Some(provider));
        let tls_cfg = match self.config.as_ref().map(|config| &config.core) {
            Some(config) => match (&config.tls_certificate_path, &config.tls_private_key_path) {
                (Some(cert), Some(key)) => match tls_config_from_files(cert, key) {
                    Ok(cfg) => Some(cfg),
                    Err(e) => {
                        eprintln!("TLS config load failed: {}; running without TLS", e);
                        None
                    }
                },
                _ => None,
            },
            None => None,
        };
        let network: Arc<dyn INetwork> = NetworkDriver::new(
            self.clone(),
            storage.clone(),
            security.clone(),
            signaler.clone(),
            fed.clone(),
            chain.clone(),
            tls_cfg,
        );
        let vmm: Arc<dyn IWorkloads> = NodeWorkloads::new(self.clone());

        // Stage 2 — federation needs storage/signaler.
        fed.second_stage(storage.clone(), signaler.clone());

        // Load the server private key for signing.
        let pem = security.fetch_key_pair("server_key");
        if let Some(first) = pem.into_iter().next()
            && let Ok(key) = Core::parse_private_key(&first)
        {
            *self.priv_key.lock().unwrap() = Some(Arc::new(key));
        }

        // Cross-protocol client-request rate limiter. One instance is shared by
        // every client-facing transport (TCP / WS / HTTP ingress) so a client's
        // quota is unified across protocols.
        let rate_limiter: Arc<dyn IRateLimiter> = match self.config.as_ref() {
            Some(config) => crate::adapters::ratelimit::RateLimiter::from_typed(&config.rate_limit),
            None => crate::adapters::ratelimit::RateLimiter::new(Default::default()),
        };

        // Install tools + chain restore.
        let tools: Arc<dyn ITools> = Arc::new(Tools {
            security,
            signaler,
            storage,
            network: network.clone(),
            vmm,
            rate_limiter,
        });
        *self.tools.lock().unwrap() = Some(tools);
        network.chain().restore_from_storage();

        // Chain submission goes through the chain module's own queue.
        // Globe.
        let sign_fn: crate::legacy::globe::SignPacketFn = {
            let me = self.clone();
            Arc::new(move |data| me.sign_packet(data))
        };
        let submit_fn: crate::legacy::globe::SubmitChainPacketFn = {
            let chain_for_submit = chain.clone();
            Arc::new(move |chain_id: &str, op: ChainPacketOp| {
                chain_for_submit.submit_chain_op(chain_id, op);
            })
        };
        let set_chain_callback_fn: crate::legacy::globe::SetChainCallbackFn = {
            let chain_for_cb = chain.clone();
            Arc::new(move |callback_id: &str, cb: ChainCallback| {
                chain_for_cb.register_chain_callback(callback_id, cb);
            })
        };
        let set_message_cb_fn: crate::legacy::globe::SetMessageCbFn = {
            let chain_for_msg = chain.clone();
            Arc::new(move |callback_id: &str, cb: MessageCallback| {
                chain_for_msg.register_message_callback(callback_id, cb);
            })
        };
        let globe = {
            // The globe is pure chain-RPC transport; the consensus provider
            // (composed above) owns governance and finance.
            Globe::new(
                self.id.clone(),
                sign_fn,
                submit_fn,
                set_chain_callback_fn,
                set_message_cb_fn,
            )
        };
        *self.globe.lock().unwrap() = Some(globe.clone());

        // Wire the chain pipeline so committed blocks flow through
        // `handle_chain_packet`.
        let trans = self.clone();
        let pipeline: crate::models::ports::PipelineFn = Box::new(
            move |txs: Vec<Vec<u8>>, insider_cb: Box<dyn Fn(Vec<u8>) + Send + Sync>| {
                let mut machine_ids: Vec<String> = Vec::new();
                for tx in txs {
                    let s = String::from_utf8_lossy(&tx);
                    let first_index = match s.find("::") {
                        Some(i) => i,
                        None => continue,
                    };
                    let typ = &s[..first_index];
                    let body = &tx[first_index + 2..];
                    if typ == "nodeJoined" {
                        insider_cb(tx.clone());
                    } else if typ == format!("sharderMap|{}", trans.id) {
                        insider_cb(tx.clone());
                    } else {
                        let r = trans.handle_chain_packet(typ, body);
                        if !r.is_empty() {
                            machine_ids.push(r);
                        }
                    }
                }
                machine_ids
            },
        );
        network.chain().register_pipeline(pipeline);

        // The chain module owns its submission queue and the framing drain; no
        // node thread is needed here.

        // The hourly election is driven by the consensus provider's autonomous
        // governance scheduler (spawned above); no node thread starts elections.

        Ok(())
    }
}

// Kept for import calm on the composition-root type contracts.
const _: fn() -> Option<Arc<AsemanConfig>> = || None;
const _: fn() -> Option<RsaPrivateKey> = || None;
