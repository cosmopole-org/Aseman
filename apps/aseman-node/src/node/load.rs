//! Starting a node's components: storage, federation, the session hub, security,
//! the consensus provider and chain, the network, workloads, and the rate limiter.

use std::sync::Arc;

use anyhow::Result;

use crate::identity::Security;
use crate::live::hub::Signaler;
use crate::node::{Node, Tools};
use crate::ratelimit::RateLimiter;
use crate::storage::NodeStorage;
use crate::transports::Network as NetworkDriver;
use crate::transports::Network;
use crate::transports::chain::callbacks::ChainCallback;
use crate::transports::chain::globe::{ChainPacketOp, Globe};
use crate::transports::chain::{Blockchain, ChainSettings};
use crate::transports::federation::FedNet;
use crate::workloads::NodeWorkloads;
use aseman_network_shell::tls_config_from_files;
use aseman_ports::consensus::ConsensusProvider as _;

impl Node {
    /// Runtime start phase invoked after load/module initialization.
    /// Start the node's components: storage, federation, the signaler, security,
    /// the consensus provider and chain, the network, workloads, and the rate
    /// limiter. Run once, after construction.
    ///
    /// # Errors
    ///
    /// Storage that cannot open, or configured TLS that cannot load.
    pub fn load(self: &Arc<Self>) -> Result<()> {
        let storage_root = self.config.storage.root_path.as_str();
        let base_db_path = self.config.storage.base_db_path.as_str();
        // Stage 1 of federation must run before the rest so we can pass
        // the same `Arc<FedNet>` into the storage / network drivers.
        let fed: Arc<FedNet> = FedNet::first_stage(self.clone());
        let storage: Arc<NodeStorage> = NodeStorage::new(
            storage_root,
            crate::storage::open_from_config(Some(&self.config), storage_root, base_db_path, true)?,
        );
        let signaler: Arc<Signaler> = Signaler::new(self.clone(), fed.clone());
        let security: Arc<Security> = Security::new(self.clone(), storage_root);

        // One consensus provider owns finance ordering AND validator
        // governance (staking/election) as the main chain's application
        // handler. It is composed once here — before the chain adapter — so
        // `Blockchain` can install its proxy as the main-chain engine handler.
        let provider = Arc::new(
            aseman_consensus_hashgraph::provider::HashgraphConsensusProvider::for_node(
                &self.id, &self.id,
            ),
        );
        for (key, value) in &self.config.consensus_properties {
            if let Err(error) = provider.set(key, value) {
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
        let chain_settings = ChainSettings::from_config(&self.config);
        let chain: Arc<crate::transports::chain::Blockchain> = Blockchain::with_consensus(
            self.clone(),
            storage_root,
            chain_settings,
            Some(provider),
            storage.consensus_logs(),
        );
        // Configured TLS that cannot be loaded is a startup failure: the
        // transports never fall back to plaintext.
        let tls_cfg = match (
            &self.config.core.tls_certificate_path,
            &self.config.core.tls_private_key_path,
        ) {
            (Some(cert), Some(key)) => Some(
                tls_config_from_files(cert, key)
                    .map_err(|error| anyhow::anyhow!("cannot load the node's TLS: {error}"))?,
            ),
            _ => None,
        };
        let network: Arc<Network> = NetworkDriver::new(
            self.clone(),
            storage.clone(),
            security.clone(),
            signaler.clone(),
            fed.clone(),
            chain.clone(),
            tls_cfg,
        );
        let vmm: Arc<NodeWorkloads> = NodeWorkloads::new(self.clone());

        // Stage 2 — federation needs storage/signaler.
        fed.second_stage(storage.clone(), signaler.clone());

        // Load the server private key for signing.
        let pem = security.fetch_key_pair("server_key");
        if let Some(first) = pem.into_iter().next()
            && let Ok(key) = Node::parse_private_key(&first)
        {
            let _ = self.node_key.set(Arc::new(key));
        }

        // Cross-protocol client-request rate limiter. One instance is shared by
        // every client-facing transport (TCP / WS / HTTP ingress) so a client's
        // quota is unified across protocols.
        let rate_limiter: Arc<RateLimiter> =
            crate::ratelimit::RateLimiter::from_typed(&self.config.rate_limit);

        // Install tools + chain restore.
        let tools: Arc<Tools> = Arc::new(Tools {
            security,
            signaler,
            storage,
            network: network.clone(),
            workloads: vmm,
            rate_limiter,
        });
        if self.tools.set(tools).is_err() {
            return Err(anyhow::anyhow!("the node is already loaded"));
        }
        network.chain().restore_from_storage();

        // Chain submission goes through the chain module's own queue.
        // Globe.
        let sign_fn: crate::transports::chain::globe::SignPacketFn = {
            let me = self.clone();
            Arc::new(move |data| me.sign_packet(data))
        };
        let submit_fn: crate::transports::chain::globe::SubmitChainPacketFn = {
            let chain_for_submit = chain.clone();
            Arc::new(move |chain_id: &str, op: ChainPacketOp| {
                chain_for_submit.submit_chain_op(chain_id, op);
            })
        };
        let set_chain_callback_fn: crate::transports::chain::globe::SetChainCallbackFn = {
            let chain_for_cb = chain.clone();
            Arc::new(move |callback_id: &str, cb: ChainCallback| {
                chain_for_cb.register_chain_callback(callback_id, cb);
            })
        };
        let globe = {
            // The globe is pure chain-RPC transport; the consensus provider
            // (composed above) owns governance and finance.
            Globe::new(self.id.clone(), sign_fn, submit_fn, set_chain_callback_fn)
        };
        let _ = self.globe.set(globe.clone());

        // Wire the chain pipeline so committed blocks flow through
        // `handle_chain_packet`.
        let trans = self.clone();
        let pipeline: crate::transports::chain::callbacks::PipelineFn = Box::new(
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
                    // Membership packets belong to the chain itself.
                    if typ == "nodeJoined" || typ == format!("sharderMap|{}", trans.id) {
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
