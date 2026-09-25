//! Translation of `chain/node/node.go` — the Babble node.
//!
//! Concurrency model: the Go node ran many goroutines sharing `core` behind
//! `coreLock`. The translation makes `Node` an `Arc<Node>` with `&self`
//! methods; `core` lives behind `Arc<Mutex<Core>>`, and the background routines
//! run on `std::thread`s holding `Arc<Node>` clones. Go's `close(ch)` broadcast
//! is reproduced by dropping the channel's sender (`Mutex<Option<Sender>>`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};

use super::control_timer::ControlTimer;
use super::core::Core;
use super::state::{Manager, State};
use super::validator::Validator;
use crate::config::Config;
use crate::hashgraph::{self as hg, Block, Store, WireEvent};
use crate::logrus::Entry;
use crate::net::{Rpc, RpcCommand, Transport};
use crate::peers::{Peer, PeerSet};
use crate::proxy::AppProxy;

/// A Babble node, implemented as a state machine.
pub struct Node {
    /// The embedded node state manager.
    pub manager: Manager,
    pub workchain_id: String,
    pub shardchain_id: String,
    pub on_new_node: Box<dyn Fn(String) + Send + Sync>,

    pub conf: Arc<Config>,
    pub logger: Entry,

    /// The link between the node and the underlying hashgraph.
    pub core: Arc<Mutex<Core>>,

    /// Transmits and receives commands to/from other nodes.
    pub trans: Arc<dyn Transport>,
    pub net_ch: Receiver<Rpc>,

    /// The link between the node and the application.
    pub proxy: Arc<dyn AppProxy>,

    /// Incoming transactions to be submitted to Babble.
    pub submit_ch: Receiver<Vec<u8>>,

    shutdown_tx: Mutex<Option<Sender<()>>>,
    shutdown_rx: Receiver<()>,
    suspend_tx: Mutex<Option<Sender<()>>>,
    suspend_rx: Receiver<()>,

    /// Periodically signals the gossip routines.
    pub control_timer: Arc<ControlTimer>,

    /// Event-driven gossip trigger. Senders use `try_send`; the bounded
    /// capacity of 1 coalesces bursts into a single pending wake-up so the
    /// babble loop never queues more than one gossip cycle ahead.
    gossip_trigger_tx: Sender<()>,
    gossip_trigger_rx: Receiver<()>,

    start: Instant,
    sync_requests: AtomicI64,
    sync_errors: AtomicI64,

    /// Number of undetermined events recorded at node initialisation.
    initial_undetermined_events: AtomicI64,
}

impl Node {
    /// Instantiates a new `Node` and initialises its hashgraph.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        conf: Arc<Config>,
        validator: Validator,
        peers: Arc<PeerSet>,
        genesis_peers: Arc<PeerSet>,
        store: Box<dyn Store>,
        trans: Arc<dyn Transport>,
        proxy: Arc<dyn AppProxy>,
        work_chain_id: &str,
        shard_chain_id: &str,
        on_new_node: Box<dyn Fn(String) + Send + Sync>,
    ) -> Arc<Node> {
        let commit_proxy = proxy.clone();
        let commit_callback: crate::proxy::CommitCallback =
            Box::new(move |block: Block| commit_proxy.commit_block(block));

        let core = Core::new(
            validator,
            peers,
            genesis_peers,
            store,
            commit_callback,
            conf.maintenance_mode,
            conf.logger(),
        );

        let net_ch = trans.consumer(work_chain_id, shard_chain_id);
        let submit_ch = proxy.submit_ch();

        let (shutdown_tx, shutdown_rx) = unbounded();
        let (suspend_tx, suspend_rx) = unbounded();
        let (gossip_trigger_tx, gossip_trigger_rx) = bounded(1);

        Arc::new(Node {
            manager: Manager::new(),
            workchain_id: work_chain_id.to_string(),
            shardchain_id: shard_chain_id.to_string(),
            on_new_node,
            logger: conf.logger(),
            conf,
            core: Arc::new(Mutex::new(core)),
            trans,
            net_ch,
            proxy,
            submit_ch,
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            shutdown_rx,
            suspend_tx: Mutex::new(Some(suspend_tx)),
            suspend_rx,
            control_timer: Arc::new(ControlTimer::new_random()),
            gossip_trigger_tx,
            gossip_trigger_rx,
            start: Instant::now(),
            sync_requests: AtomicI64::new(0),
            sync_errors: AtomicI64::new(0),
            initial_undetermined_events: AtomicI64::new(0),
        })
    }

    // ----- Public methods -----

    /// Controls the bootstrap process and sets the node's initial state.
    pub fn init(&self) -> Result<()> {
        if self.conf.bootstrap {
            self.logger.debug("Bootstrap");
            self.core.lock().unwrap().bootstrap()?;
            self.logger.debug("Bootstrap completed");
        }

        if !self.conf.maintenance_mode {
            self.logger.debug("Start Listening");
            let trans = self.trans.clone();
            std::thread::spawn(move || trans.listen());

            let in_peer_set = {
                let core = self.core.lock().unwrap();
                core.peers.by_id.contains_key(&core.validator.id())
            };
            if in_peer_set {
                self.logger.debug("Node belongs to PeerSet");
                self.set_babbling_or_catching_up_state();
            } else {
                self.logger
                    .debug("Node does not belong to PeerSet => Joining");
                self.transition(State::Joining);
            }
        } else {
            self.transition(State::Suspended);
        }

        let undet = self.core.lock().unwrap().get_undetermined_events().len() as i64;
        self.initial_undetermined_events
            .store(undet, Ordering::SeqCst);
        Ok(())
    }

    /// The main loop of the node.
    pub fn run(self: &Arc<Self>, gossip: bool) {
        if self.conf.maintenance_mode {
            return;
        }

        let ct = self.control_timer.clone();
        let heartbeat = self.conf.heartbeat_timeout;
        std::thread::spawn(move || ct.run(heartbeat));

        let bg = self.clone();
        std::thread::spawn(move || bg.do_background_work());

        loop {
            match self.manager.get_state() {
                State::Babbling => self.babble(gossip),
                State::CatchingUp => {
                    let _ = self.fast_forward();
                }
                State::Joining => {
                    let _ = self.join();
                }
                State::Suspended => std::thread::sleep(Duration::from_millis(2000)),
                State::Shutdown => return,
                // Go's `Run` switch has no `Leaving` case (it falls through);
                // the node never transitions to `Leaving` within `Run`.
                State::Leaving => {}
            }
        }
    }

    /// Runs the node on a separate thread.
    pub fn run_async(self: &Arc<Self>, gossip: bool) {
        self.logger.with_field("gossip", gossip).debug("runasync");
        let n = self.clone();
        std::thread::spawn(move || n.run(gossip));
    }

    /// Politely leaves the network and waits to be removed from the
    /// validator-set via consensus.
    pub fn leave(&self) -> Result<()> {
        if self.conf.maintenance_mode {
            return Ok(());
        }
        self.logger.info("LEAVING");
        let result = {
            let mut core = self.core.lock().unwrap();
            core.leave(self.conf.join_timeout)
        };
        if let Err(e) = &result {
            self.logger.with_error(e).error("Leaving");
        }
        self.shutdown();
        result
    }

    /// Cleanly shuts the node down.
    pub fn shutdown(&self) {
        if self.manager.get_state() != State::Shutdown {
            self.logger.info("SHUTDOWN");
            self.transition(State::Shutdown);
            // Broadcast shutdown by dropping the sender.
            *self.shutdown_tx.lock().unwrap() = None;
            self.manager.wait_routines();
            let _ = self.trans.close();
            let _ = self.core.lock().unwrap().hg.store.close();
        }
    }

    /// Puts the node in Suspended mode without closing the transport.
    pub fn suspend(&self) {
        let s = self.manager.get_state();
        if s != State::Suspended && s != State::Shutdown {
            self.logger.info("SUSPEND");
            self.transition(State::Suspended);
            *self.suspend_tx.lock().unwrap() = None;
            self.manager.wait_routines();
        }
    }

    /// The numeric ID of the node's validator.
    pub fn get_id(&self) -> u32 {
        self.core.lock().unwrap().validator.id()
    }

    /// The hex representation of the validator's public key.
    pub fn get_pub_key(&self) -> String {
        self.core.lock().unwrap().validator.public_key_hex()
    }

    /// Information about the node.
    pub fn get_stats(&self) -> HashMap<String, String> {
        let core = self.core.lock().unwrap();
        let mut s = HashMap::new();
        let lcr = core.get_last_consensus_round_index().unwrap_or(-1);
        s.insert("last_consensus_round".into(), lcr.to_string());
        s.insert(
            "last_block_index".into(),
            core.get_last_block_index().to_string(),
        );
        s.insert(
            "consensus_events".into(),
            core.get_consensus_events_count().to_string(),
        );
        s.insert(
            "undetermined_events".into(),
            core.get_undetermined_events().len().to_string(),
        );
        s.insert(
            "transactions".into(),
            core.get_consensus_transactions_count().to_string(),
        );
        s.insert(
            "transaction_pool".into(),
            core.transaction_pool.len().to_string(),
        );
        s.insert(
            "num_peers".into(),
            core.peer_selector.get_peers().len().to_string(),
        );
        s.insert(
            "last_peer_change".into(),
            core.last_peer_change_round.to_string(),
        );
        s.insert("id".into(), core.validator.id().to_string());
        s.insert("state".into(), self.manager.get_state().to_string());
        s.insert("moniker".into(), core.validator.moniker.clone());
        s
    }

    /// Returns a block by index.
    pub fn get_block(&self, block_index: i64) -> Result<Block> {
        self.core.lock().unwrap().hg.store.get_block(block_index)
    }

    /// The index of the last known block.
    pub fn get_last_block_index(&self) -> i64 {
        self.core.lock().unwrap().get_last_block_index()
    }

    /// The index of the last consensus round.
    pub fn get_last_consensus_round_index(&self) -> i64 {
        self.core
            .lock()
            .unwrap()
            .get_last_consensus_round_index()
            .unwrap_or(-1)
    }

    /// The list of currently known peers.
    pub fn get_peers(&self) -> Vec<Peer> {
        self.core.lock().unwrap().peers.peers.clone()
    }

    /// Attach an external peer-host mirror to the consensus core so it is kept
    /// current on every `set_peers`. Lets readers outside the engine obtain the
    /// live peer set without locking `core`. See `Core::peer_host_cache`.
    pub fn attach_peer_cache(&self, cache: Arc<Mutex<Vec<String>>>) {
        self.core.lock().unwrap().attach_peer_cache(cache);
    }

    /// The validator-set authoritative at a given round.
    pub fn get_validator_set(&self, round: i64) -> Result<Vec<Peer>> {
        let core = self.core.lock().unwrap();
        let peer_set = core.hg.store.get_peer_set(round)?;
        Ok(peer_set.peers.clone())
    }

    /// The entire history of validator-sets per round.
    pub fn get_all_validator_sets(&self) -> Result<HashMap<i64, Vec<Peer>>> {
        self.core.lock().unwrap().hg.store.get_all_peer_sets()
    }

    // ----- Background -----

    /// Continuously listens to incoming transactions, gossip and shutdown.
    fn do_background_work(self: Arc<Self>) {
        loop {
            select! {
                recv(self.net_ch) -> rpc => {
                    match rpc {
                        Ok(rpc) => {
                            // RPCs that ingest new events (`EagerSync`) or
                            // queue a new internal-transaction (`Join`) leave
                            // us with fresh content other peers haven't seen.
                            // `Sync` and `FastForward` are read-only on our
                            // side, so they don't warrant a wake-up.
                            let kind_triggers_gossip = matches!(
                                &rpc.command,
                                RpcCommand::EagerSync(_) | RpcCommand::Join(_)
                            );
                            let n = self.clone();
                            self.manager.go_func(Box::new(move || {
                                n.process_rpc(rpc);
                                if kind_triggers_gossip {
                                    n.trigger_gossip();
                                }
                                n.reset_timer();
                            }));
                        }
                        Err(_) => return,
                    }
                }
                recv(self.submit_ch) -> t => {
                    match t {
                        Ok(t) => {
                            self.logger.debug("Adding Transaction");
                            self.add_transaction(t);
                            self.trigger_gossip();
                            self.reset_timer();
                        }
                        Err(_) => return,
                    }
                }
                recv(self.shutdown_rx) -> _ => return,
            }
        }
    }

    /// Resets the control timer to the heartbeat timeout, slowing it down when
    /// the node is not busy.
    pub fn reset_timer(&self) {
        if !self.control_timer.is_set() {
            let mut ts = self.conf.heartbeat_timeout;
            let busy = self.core.lock().unwrap().busy();
            if !busy {
                ts = self.conf.slow_heartbeat_timeout;
            }
            self.control_timer.reset(ts);
        }
    }

    /// Suspends the node when too many undetermined events accumulate or when
    /// the validator has been evicted.
    fn check_suspend(&self) {
        let (too_many, evicted) = {
            let core = self.core.lock().unwrap();
            let new_undetermined = core.get_undetermined_events().len() as i64
                - self.initial_undetermined_events.load(Ordering::SeqCst);
            let too_many = new_undetermined > self.conf.suspend_limit * core.validators.len();
            let evicted = core.hg.last_consensus_round.is_some()
                && core.removed_round > 0
                && core.removed_round > core.accepted_round
                && core.hg.last_consensus_round.unwrap() >= core.removed_round;
            (too_many, evicted)
        };
        if too_many || evicted {
            self.logger
                .with_field("evicted", evicted)
                .with_field("tooManyUndeterminedEvents", too_many)
                .debug("SUSPEND");
            self.suspend();
        }
    }

    // ----- Babbling -----

    /// Signals the babble loop to perform a gossip cycle as soon as possible.
    /// Coalescing: if a wake-up is already pending, this is a no-op.
    pub fn trigger_gossip(&self) {
        let _ = self.gossip_trigger_tx.try_send(());
    }

    /// Dispatches a single gossip cycle (or monologue when alone).
    fn dispatch_gossip(self: &Arc<Self>) {
        let peer = self.core.lock().unwrap().peer_selector.next();
        match peer {
            Some(peer) => {
                let n = self.clone();
                self.manager.go_func(Box::new(move || n.gossip(peer)));
            }
            None => {
                let _ = self.monologue();
            }
        }
    }

    /// Initiates gossip or monologue. The heartbeat timer provides a slow
    /// fallback so the node still makes progress when idle; `gossip_trigger`
    /// is fired by callers that produce new events (incoming pushes, locally
    /// submitted transactions, fresh pulls) so propagation happens in real
    /// time instead of waiting for the next tick.
    fn babble(self: &Arc<Self>, gossip: bool) {
        self.logger.info("BABBLING");
        loop {
            select! {
                recv(self.control_timer.tick_rx) -> _ => {
                    if gossip {
                        self.dispatch_gossip();
                    }
                    self.reset_timer();
                    self.check_suspend();
                }
                recv(self.gossip_trigger_rx) -> _ => {
                    if gossip {
                        self.dispatch_gossip();
                    }
                    self.reset_timer();
                    self.check_suspend();
                }
                recv(self.suspend_rx) -> _ => return,
                recv(self.shutdown_rx) -> _ => return,
            }
        }
    }

    /// Records events when the node is alone in the network.
    fn monologue(&self) -> Result<()> {
        let mut core = self.core.lock().unwrap();
        if core.busy() {
            if let Err(e) = core.add_self_event("") {
                self.logger
                    .with_error(&e)
                    .error("monologue, AddSelfEvent()");
                return Err(e);
            }
            if let Err(e) = core.process_sig_pool() {
                self.logger
                    .with_error(&e)
                    .error("monologue, ProcessSigPool()");
                return Err(e);
            }
        }
        Ok(())
    }

    /// Performs a pull-push gossip operation with the selected peer.
    fn gossip(&self, peer: Peer) {
        let result = (|| -> Result<()> {
            let other_known = self.pull(&peer)?;
            self.push(&peer, other_known)?;
            self.log_stats();
            Ok(())
        })();

        let connected = result.is_ok();
        if let Err(e) = &result {
            self.logger.with_error(e).warn("gossip");
        }

        let new_connection = {
            let mut core = self.core.lock().unwrap();
            core.peer_selector.update_last(peer.id(), connected)
        };
        if new_connection {
            self.logger
                .with_field("peer_ID", peer.id())
                .with_field("peer_moniker", &peer.moniker)
                .info("Connected");
        }
    }

    /// Performs a SyncRequest and processes the response.
    fn pull(&self, peer: &Peer) -> Result<HashMap<u32, i64>> {
        let known_events = self.core.lock().unwrap().known_events();

        let resp = self
            .request_sync(&peer.net_addr, known_events, self.conf.sync_limit)
            .map_err(|e| {
                self.logger.with_error(&e).warn("requestSync()");
                e
            })?;

        self.logger
            .with_field("from_id", resp.from_id)
            .with_field("events", resp.events.len())
            .debug("SyncResponse");

        let received = resp.events.len();
        {
            let mut core = self.core.lock().unwrap();
            self.sync_with_core(&mut core, peer.id(), resp.events)?;
        }
        // New events from this peer likely aren't known to the rest of the
        // network yet — wake the babble loop so they propagate immediately
        // instead of waiting for the next heartbeat.
        if received > 0 {
            self.trigger_gossip();
        }
        Ok(resp.known)
    }

    /// Performs an EagerSyncRequest.
    fn push(&self, peer: &Peer, known_events: HashMap<u32, i64>) -> Result<()> {
        let mut event_diff = {
            let mut core = self.core.lock().unwrap();
            core.event_diff(known_events)?
        };

        if !event_diff.is_empty() {
            if (self.conf.sync_limit as usize) < event_diff.len() {
                self.logger
                    .with_field("sync_limit", self.conf.sync_limit)
                    .with_field("diff_length", event_diff.len())
                    .debug("Push sync_limit");
                event_diff.truncate(self.conf.sync_limit as usize);
            }

            let wire_events = self.core.lock().unwrap().to_wire(&event_diff);
            let resp2 = self
                .request_eager_sync(&peer.net_addr, wire_events)
                .map_err(|e| {
                    self.logger.with_error(&e).warn("requestEagerSync()");
                    e
                })?;
            self.logger
                .with_field("from_id", resp2.from_id)
                .with_field("success", resp2.success)
                .debug("EagerSyncResponse");
        }
        Ok(())
    }

    /// Inserts events into the hashgraph and processes the signature pool.
    pub fn sync_with_core(
        &self,
        core: &mut Core,
        from_id: u32,
        events: Vec<WireEvent>,
    ) -> Result<()> {
        if let Err(e) = core.sync(from_id, events) {
            if !hg::is_normal_self_parent_error(&e) {
                self.logger.with_error(&e).error("");
                return Err(e);
            }
        }
        if let Err(e) = core.process_sig_pool() {
            self.logger.with_error(&e).error("");
            return Err(e);
        }
        Ok(())
    }

    // ----- CatchingUp -----

    /// Enacts the CatchingUp state.
    fn fast_forward(&self) -> Result<()> {
        self.logger.info("CATCHING-UP");
        self.manager.wait_routines();

        let resp = match self.get_best_fast_forward_response() {
            Some(resp) => resp,
            None => {
                self.logger
                    .error("getBestFastForwardResponse returned nil => Babbling");
                self.transition(State::Babbling);
                return Err(anyhow::anyhow!("getBestFastForwardResponse returned nil"));
            }
        };

        if let Err(e) = self.proxy.restore(&resp.snapshot) {
            self.logger
                .with_error(&e)
                .error("Restoring App from Snapshot");
            return Err(e);
        }

        {
            let mut core = self.core.lock().unwrap();
            if let Err(e) = core.fast_forward(&resp.block, &resp.frame) {
                self.logger
                    .with_error(&e)
                    .error("Fast Forwarding Hashgraph");
                return Err(e);
            }
            if let Err(e) = core.process_accepted_internal_transactions(
                resp.block.round_received(),
                resp.block.internal_transaction_receipts().to_vec(),
            ) {
                self.logger
                    .with_error(&e)
                    .error("Processing AnchorBlock InternalTransactionReceipts");
            }
        }

        self.logger.debug("FastForward OK");
        self.transition(State::Babbling);
        Ok(())
    }

    /// Performs a FastForwardRequest with all known peers and picks the
    /// response with the highest block number.
    fn get_best_fast_forward_response(&self) -> Option<crate::net::FastForwardResponse> {
        let mut best_response = None;
        let mut max_block: i64 = 0;

        let peers = self.core.lock().unwrap().peer_selector.get_peers();
        for p in &peers.peers {
            let resp = match self.request_fast_forward(&p.net_addr) {
                Ok(resp) => resp,
                Err(e) => {
                    self.logger.with_error(&e).error("requestFastForward()");
                    continue;
                }
            };
            self.logger
                .with_field("from_id", resp.from_id)
                .with_field("block_index", resp.block.index())
                .debug("FastForwardResponse");
            if resp.block.index() > max_block {
                max_block = resp.block.index();
                best_response = Some(resp);
            }
        }
        best_response
    }

    // ----- Joining -----

    /// Adds the node's validator key to the validator-set via consensus.
    fn join(&self) -> Result<()> {
        if self.conf.maintenance_mode {
            return Ok(());
        }
        self.logger.info("JOINING");

        let peer = match self.core.lock().unwrap().peer_selector.next() {
            Some(p) => p,
            None => return Err(anyhow::anyhow!("no peer to join with")),
        };

        let resp = match self.request_join(&peer.net_addr) {
            Ok(resp) => resp,
            Err(e) => {
                self.logger
                    .error(format!("Cannot join: {} {}", peer.net_addr, e));
                return Err(e);
            }
        };

        self.logger
            .with_field("from_id", resp.from_id)
            .with_field("accepted", resp.accepted)
            .with_field("accepted_round", resp.accepted_round)
            .with_field("peers", resp.peers.len())
            .debug("JoinResponse");

        if resp.accepted {
            {
                let mut core = self.core.lock().unwrap();
                core.accepted_round = resp.accepted_round;
                core.removed_round = -1;
            }
            self.set_babbling_or_catching_up_state();
        } else {
            self.logger.info("JoinRequest rejected");
            self.shutdown();
        }
        Ok(())
    }

    // ----- Utils -----

    /// Changes the node state and notifies the app via the proxy.
    pub fn transition(&self, state: State) {
        self.manager.set_state(state);
        if let Err(e) = self.proxy.on_state_changed(state) {
            self.logger.error(e);
        }
    }

    /// Sets the node to CatchingUp if fast-sync is enabled, else Babbling.
    fn set_babbling_or_catching_up_state(&self) {
        if self.conf.enable_fast_sync {
            self.logger.debug("FastSync enabled => CatchingUp");
            self.transition(State::CatchingUp);
        } else {
            self.logger.debug("FastSync not enabled => Babbling");
            let mut core = self.core.lock().unwrap();
            if core.set_head_and_seq().is_err() {
                let _ = core.set_head_and_seq();
            }
            drop(core);
            self.transition(State::Babbling);
        }
    }

    /// Thread-safe addition of an incoming transaction to the core's pool.
    fn add_transaction(&self, tx: Vec<u8>) {
        self.core.lock().unwrap().add_transactions(vec![tx]);
    }

    /// Logs the node statistics.
    fn log_stats(&self) {
        let stats = self.get_stats();
        let mut entry = self.logger.clone();
        for k in [
            "last_consensus_round",
            "last_block_index",
            "undetermined_events",
            "transactions",
            "transaction_pool",
            "num_peers",
            "id",
            "state",
            "moniker",
        ] {
            if let Some(v) = stats.get(k) {
                entry = entry.with_field(k, v);
            }
        }
        entry.debug("Stats");
    }
}
