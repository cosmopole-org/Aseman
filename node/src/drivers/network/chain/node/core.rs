//! Translation of `chain/node/core.go`.
//!
//! `Core` is the object the `Node` uses to manipulate the hashgraph indirectly.
//!
//! Reentrancy note: in Go, `Core.commit` was passed as the hashgraph's commit
//! callback and re-entered `Core` while the hashgraph held it. Rust forbids
//! that ownership cycle, so the hashgraph's commit callback instead pushes
//! committed blocks onto a shared queue, and `Core` drains that queue
//! (`drain_commits`) right after every consensus run — behaviourally
//! equivalent, since a committed block is always processed before the next
//! sync round.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossbeam_channel::Receiver;

use super::peer_selector::{PeerSelector, RandomPeerSelector};
use super::promise::{JoinPromise, JoinPromiseResponse};
use super::validator::Validator;
use crate::compat::logrus::Entry;
use crate::drivers::network::chain::common::{self, is_store, StoreErrType};
use crate::drivers::network::chain::hashgraph::{
    self as hg, Block, BlockSignature, Event, Frame, Hashgraph, InternalTransaction,
    InternalTransactionReceipt, SigPool, Store, TransactionType, WireEvent,
};
use crate::drivers::network::chain::peers::PeerSet;
use crate::drivers::network::chain::proxy::CommitCallback;

/// Manipulates the hashgraph on behalf of the `Node`.
pub struct Core {
    /// Wraps the private key controlling this node.
    pub validator: Validator,
    /// The underlying hashgraph.
    pub hg: Hashgraph,
    /// The validator-set the hashgraph was initialised with.
    pub genesis_peers: Arc<PeerSet>,
    /// The latest validator-set used in the consensus methods.
    pub validators: Arc<PeerSet>,
    /// The peers this node tries to gossip with.
    pub peers: Arc<PeerSet>,
    /// Decides which peer to talk to next.
    pub peer_selector: Box<dyn PeerSelector>,
    pub selector_lock: Mutex<()>,
    /// Hash of this instance's head Event.
    pub head: String,
    /// Index of this instance's head Event.
    pub seq: i64,
    /// First round at which the node's last join request takes effect.
    pub accepted_round: i64,
    /// Round at which the node's last leave request takes effect.
    pub removed_round: i64,
    /// Minimum consensus round the node needs to reach.
    pub target_round: i64,
    /// Updated whenever a join/leave request is accepted.
    pub last_peer_change_round: i64,
    /// Events not tied to this node's head, managed by `sync`.
    pub heads: HashMap<u32, Option<Event>>,
    /// Transactions submitted from the app, not yet in the hashgraph.
    pub transaction_pool: Vec<Vec<u8>>,
    /// Internal transactions, not yet in the hashgraph.
    pub internal_transaction_pool: Vec<InternalTransaction>,
    /// Block signatures created by this node, not yet in the hashgraph.
    pub self_block_signatures: SigPool,
    /// Called by the hashgraph (indirectly) when a block is committed.
    pub proxy_commit_callback: CommitCallback,
    /// Whether the node is in maintenance mode (disables leave requests).
    pub maintenance_mode: bool,
    /// Pending JoinRequests while their InternalTransactions go through
    /// consensus asynchronously.
    pub promises: HashMap<String, JoinPromise>,
    /// Queue the hashgraph's commit callback pushes committed blocks onto.
    pub committed_blocks: Arc<Mutex<Vec<Block>>>,
    /// Optional external mirror of the current peer hosts (`host` of each
    /// peer's `net_addr`). When set, it is refreshed on every `set_peers` so
    /// readers outside the consensus engine (e.g. `Blockchain::peers()`, called
    /// from the commit handler while this `core` mutex is held) can obtain the
    /// live peer set from an independent lock instead of re-locking `core`.
    pub peer_host_cache: Option<Arc<Mutex<Vec<String>>>>,
    pub logger: Entry,
}

/// Host portions (`host` of `host:port`) of every peer in `ps`.
fn peer_hosts_of(ps: &PeerSet) -> Vec<String> {
    ps.peers
        .iter()
        .map(|p| {
            p.net_addr
                .split(':')
                .next()
                .unwrap_or(&p.net_addr)
                .to_string()
        })
        .collect()
}

impl Core {
    /// Creates and initialises a new `Core`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        validator: Validator,
        peers: Arc<PeerSet>,
        genesis_peers: Arc<PeerSet>,
        store: Box<dyn Store>,
        proxy_commit_callback: CommitCallback,
        maintenance_mode: bool,
        logger: Entry,
    ) -> Core {
        let peer_selector: Box<dyn PeerSelector> =
            Box::new(RandomPeerSelector::new(peers.clone(), validator.id()));

        let committed_blocks: Arc<Mutex<Vec<Block>>> = Arc::new(Mutex::new(Vec::new()));
        let cb = committed_blocks.clone();
        let commit_callback: hg::InternalCommitCallback = Box::new(move |block: &Block| {
            cb.lock().unwrap().push(block.clone());
            Ok(())
        });

        let mut hashgraph = Hashgraph::new(store, commit_callback, Some(logger.clone()));
        let _ = hashgraph.init(genesis_peers.clone());

        Core {
            validator,
            hg: hashgraph,
            genesis_peers: genesis_peers.clone(),
            validators: genesis_peers,
            peers,
            peer_selector,
            selector_lock: Mutex::new(()),
            head: String::new(),
            seq: -1,
            accepted_round: -1,
            removed_round: -1,
            target_round: -1,
            last_peer_change_round: -1,
            heads: HashMap::new(),
            transaction_pool: Vec::new(),
            internal_transaction_pool: Vec::new(),
            self_block_signatures: SigPool::new(),
            proxy_commit_callback,
            maintenance_mode,
            promises: HashMap::new(),
            committed_blocks,
            peer_host_cache: None,
            logger,
        }
    }

    /// Attach an external peer-host mirror and seed it with the current peers.
    /// The cache is refreshed by every subsequent `set_peers`, so it tracks
    /// dynamic membership. See `Core::peer_host_cache`.
    pub fn attach_peer_cache(&mut self, cache: Arc<Mutex<Vec<String>>>) {
        *cache.lock().unwrap() = peer_hosts_of(&self.peers);
        self.peer_host_cache = Some(cache);
    }

    /// Sets the head and seq of this `Core`.
    pub fn set_head_and_seq(&mut self) -> Result<()> {
        let mut head = String::new();
        let mut seq: i64 = -1;

        if self
            .hg
            .store
            .repertoire_by_id()
            .contains_key(&self.validator.id())
        {
            match self
                .hg
                .store
                .last_event_from(&self.validator.public_key_hex())
            {
                Ok(last) => {
                    if !last.is_empty() {
                        let last_event = self.get_event(&last)?;
                        head = last;
                        seq = last_event.index();
                    }
                }
                Err(e) => {
                    if !is_store(&e, StoreErrType::Empty) {
                        return Err(e);
                    }
                }
            }
        } else {
            self.logger.debug("Not in repertoire yet.");
        }

        self.head = head;
        self.seq = seq;
        self.logger
            .with_field("core.Head", &self.head)
            .with_field("core.Seq", self.seq)
            .debug("SetHeadAndSeq");
        Ok(())
    }

    /// Calls the hashgraph `Bootstrap`.
    pub fn bootstrap(&mut self) -> Result<()> {
        self.logger.debug("Bootstrap");
        self.hg.bootstrap()
    }

    /// Sets the peers and a new `RandomPeerSelector`.
    pub fn set_peers(&mut self, ps: Arc<PeerSet>) {
        self.peers = ps;
        // Keep the external peer-host mirror current so commit-time readers see
        // membership changes (see `Core::peer_host_cache`).
        if let Some(cache) = &self.peer_host_cache {
            *cache.lock().unwrap() = peer_hosts_of(&self.peers);
        }
        self.peer_selector = Box::new(RandomPeerSelector::new(
            self.peers.clone(),
            self.validator.id(),
        ));
    }

    /// Indicates whether there is unfinished work.
    pub fn busy(&self) -> bool {
        self.hg.pending_loaded_events > 0
            || !self.transaction_pool.is_empty()
            || !self.internal_transaction_pool.is_empty()
            || !self.self_block_signatures.is_empty()
            || self
                .hg
                .last_consensus_round
                .map(|lcr| lcr < self.target_round)
                .unwrap_or(false)
    }

    /// Decodes and inserts new Events into the hashgraph. The unknown events
    /// are expected to be in topological order.
    pub fn sync(&mut self, from_id: u32, unknown_events: Vec<WireEvent>) -> Result<()> {
        self.logger
            .with_field("unknown_events", unknown_events.len())
            .debug("Sync");

        let mut other_head: Option<Event> = None;
        for we in &unknown_events {
            let mut ev = match self.hg.read_wire_info(we) {
                Ok(ev) => ev,
                Err(e) => {
                    self.logger.with_error(&e).error("Reading WireEvent");
                    return Err(e);
                }
            };

            if let Err(e) = self.insert_event_and_run_consensus(&mut ev, false) {
                if hg::is_normal_self_parent_error(&e) {
                    continue;
                } else {
                    self.logger.with_error(&e).error("Inserting Event");
                    return Err(e);
                }
            }

            if we.body.creator_id == from_id {
                other_head = Some(ev.clone());
            }

            if let Some(Some(h)) = self.heads.get(&we.body.creator_id) {
                if we.body.index > h.index() {
                    self.heads.remove(&we.body.creator_id);
                }
            }
        }

        // Do not overwrite a non-empty head with an empty head.
        let overwrite = match self.heads.get(&from_id) {
            None | Some(None) => true,
            Some(Some(h)) => other_head
                .as_ref()
                .map(|oh| oh.index() > h.index())
                .unwrap_or(false),
        };
        if overwrite {
            self.heads.insert(from_id, other_head);
        }

        // Process heads when we actually have a new event from a peer to
        // acknowledge, or when there's pending work, or at bootstrap. Without
        // this, on an idle network the hashgraph never advances rounds and
        // consensus stalls. We check for Some-valued heads (we already
        // inserted a None placeholder for from_id above) so empty syncs
        // don't trigger spurious self-events.
        let has_real_head = self.heads.values().any(|v| v.is_some());
        if has_real_head || self.busy() || self.seq < 0 {
            return self.record_heads();
        }
        Ok(())
    }

    /// Adds the recorded heads as self-events.
    pub fn record_heads(&mut self) -> Result<()> {
        self.logger
            .with_field("heads", self.heads.len())
            .debug("RecordHeads()");

        let ids: Vec<u32> = self.heads.keys().copied().collect();
        for id in ids {
            let op = match self.heads.get(&id) {
                Some(Some(ev)) => ev.hex(),
                _ => String::new(),
            };
            self.add_self_event(&op)?;
            self.heads.remove(&id);
        }

        // If there are pending transactions but no heads to consume, still
        // create a solo self-event so the transaction can be gossiped and
        // committed. Without this, transactions submitted on a fully-synced
        // network would never make it into the hashgraph.
        if !self.transaction_pool.is_empty()
            || !self.internal_transaction_pool.is_empty()
            || !self.self_block_signatures.is_empty()
        {
            self.add_self_event("")?;
        }
        Ok(())
    }

    /// Adds a self-event.
    pub fn add_self_event(&mut self, other_head: &str) -> Result<()> {
        if self.hg.store.last_round() < self.accepted_round {
            self.logger.debug(format!(
                "Too early to insert self-event ({} / {})",
                self.hg.store.last_round(),
                self.accepted_round
            ));
            return Ok(());
        }

        let sigs = self.self_block_signatures.slice();
        let txs = self.transaction_pool.len();
        let itxs = self.internal_transaction_pool.len();

        let mut new_head = Event::new(
            self.transaction_pool.clone(),
            self.internal_transaction_pool.clone(),
            sigs.clone(),
            vec![self.head.clone(), other_head.to_string()],
            self.validator.public_key_bytes(),
            self.seq + 1,
        );

        if let Err(e) = self.sign_and_insert_self_event(&mut new_head) {
            self.logger.with_error(&e).error("Error inserting new head");
            return Err(e);
        }

        self.logger
            .with_field("index", new_head.index())
            .with_field("transactions", new_head.transactions().len())
            .with_field(
                "internal_transactions",
                new_head.internal_transactions().len(),
            )
            .with_field("block_signatures", new_head.block_signatures().len())
            .debug("Created Self-Event");

        // Do not remove pool elements that were added by the commit callback.
        self.transaction_pool = self.transaction_pool.split_off(txs);
        self.internal_transaction_pool = self.internal_transaction_pool.split_off(itxs);
        self.self_block_signatures.remove_slice(&sigs);
        Ok(())
    }

    /// Signs a hashgraph event, inserts it and runs consensus.
    pub fn sign_and_insert_self_event(&mut self, event: &mut Event) -> Result<()> {
        event.sign(&self.validator.key)?;
        self.insert_event_and_run_consensus(event, true)
    }

    /// Inserts a hashgraph event and runs consensus.
    pub fn insert_event_and_run_consensus(
        &mut self,
        event: &mut Event,
        set_wire_info: bool,
    ) -> Result<()> {
        self.hg
            .insert_event_and_run_consensus(event, set_wire_info)?;
        if event.creator() == self.validator.public_key_hex() {
            self.head = event.hex();
            self.seq = event.index();
        }
        self.drain_commits();
        Ok(())
    }

    /// Returns known events from the hashgraph store.
    pub fn known_events(&self) -> HashMap<u32, i64> {
        self.hg.store.known_events()
    }

    /// Resets the hashgraph from a Block and associated Frame (FastSync).
    pub fn fast_forward(&mut self, block: &Block, frame: &Frame) -> Result<()> {
        self.logger.debug(format!("Fast Forward {}", frame.round));
        let peer_set = PeerSet::new(frame.peers.clone());

        self.hg.check_block(block, &peer_set)?;

        let frame_hash = frame.hash()?;
        if block.frame_hash() != frame_hash.as_slice() {
            return Err(anyhow!("Invalid Frame Hash"));
        }

        self.hg.reset(block, frame)?;
        self.set_head_and_seq()?;

        self.set_peers(Arc::new(PeerSet::new(frame.peers.clone())));
        self.validators = Arc::new(PeerSet::new(frame.peers.clone()));
        Ok(())
    }

    /// Returns the anchor block and its frame from the hashgraph.
    pub fn get_anchor_block_with_frame(&mut self) -> Result<(Block, Frame)> {
        self.hg.get_anchor_block_with_frame()
    }

    /// Politely leaves the network by submitting a remove `InternalTransaction`.
    pub fn leave(&mut self, leave_timeout: Duration) -> Result<()> {
        let p = match self.validators.by_id.get(&self.validator.id()) {
            Some(p) => p.clone(),
            None => {
                self.logger.debug("Leave: not a validator, do nothing");
                return Ok(());
            }
        };

        if self.validators.peers.len() <= 1 {
            self.logger.debug("Leave: alone, do nothing");
            return Ok(());
        }

        if self.maintenance_mode {
            self.logger.debug("Leave: maintenance mode, do nothing");
            return Ok(());
        }

        self.logger.debug("Leave: submit InternalTransaction");
        let mut itx = InternalTransaction::new(TransactionType::PeerRemove, p);
        itx.sign(&self.validator.key)?;
        let resp_rx = self.add_internal_transaction(itx);

        match resp_rx.recv_timeout(leave_timeout) {
            Ok(resp) => {
                self.logger
                    .with_field("leaving_round", resp.accepted_round)
                    .with_field("peers", resp.peers.len())
                    .debug("leave request processed");
            }
            Err(_) => {
                let err = anyhow!("Timeout waiting for leave request to go through consensus");
                self.logger.with_error(&err).error("");
                return Err(err);
            }
        }

        if self.peers.len() >= 1 {
            let deadline = Instant::now() + leave_timeout;
            loop {
                if Instant::now() >= deadline {
                    let err = anyhow!("Timeout waiting for leaving node to reach TargetRound");
                    self.logger.with_error(&err).error("");
                    return Err(err);
                }
                match self.hg.last_consensus_round {
                    Some(lcr) if lcr < self.removed_round => {
                        self.logger.debug(format!(
                            "Waiting to reach RemovedRound: {}/{}",
                            lcr, self.removed_round
                        ));
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    _ => return Ok(()),
                }
            }
        }
        Ok(())
    }

    /// Drains and processes the blocks the hashgraph committed via its callback.
    pub fn drain_commits(&mut self) {
        let blocks: Vec<Block> = std::mem::take(&mut *self.committed_blocks.lock().unwrap());
        for block in blocks {
            if let Err(e) = self.process_commit(block) {
                self.logger.warn(format!("Failed to commit block: {}", e));
            }
        }
    }

    /// Commits a block to the app and processes the response.
    pub fn process_commit(&mut self, mut block: Block) -> Result<()> {
        self.logger
            .with_field("block", block.index())
            .with_field("txs", block.transactions().len())
            .with_field("internal_txs", block.internal_transactions().len())
            .info("Commit");

        let commit_response = (self.proxy_commit_callback)(block.clone());
        match commit_response {
            Ok(resp) => {
                self.logger
                    .with_field("block", block.index())
                    .with_field(
                        "internal_txs_receipts",
                        resp.internal_transaction_receipts.len(),
                    )
                    .with_field("state_hash", common::encode_to_string(&resp.state_hash))
                    .info("Commit response");

                block.body.state_hash = resp.state_hash.clone();
                block.body.internal_transaction_receipts =
                    resp.internal_transaction_receipts.clone();

                let block_peer_set = self.hg.store.get_peer_set(block.round_received())?;
                if block_peer_set.by_id.contains_key(&self.validator.id()) {
                    let sig = self.sign_block(&mut block)?;
                    self.self_block_signatures.add(sig);
                }

                self.hg.set_anchor_block(&block)?;
                self.process_accepted_internal_transactions(
                    block.round_received(),
                    resp.internal_transaction_receipts,
                )?;
                Ok(())
            }
            Err(e) => {
                self.logger.with_error(&e).error("Commit response");
                Err(e)
            }
        }
    }

    /// Signs the block and saves it.
    pub fn sign_block(&mut self, block: &mut Block) -> Result<BlockSignature> {
        let sig = block.sign(&self.validator.key)?;
        block.set_signature(sig.clone())?;
        self.hg.store.set_block(block)?;
        Ok(sig)
    }

    /// Processes accepted internal-transaction receipts from a block, updating
    /// the peer-set for round `round_received + 6`.
    pub fn process_accepted_internal_transactions(
        &mut self,
        round_received: i64,
        receipts: Vec<InternalTransactionReceipt>,
    ) -> Result<()> {
        let mut current_peers = self.peers.clone();
        let mut validators = self.validators.clone();

        // +6: by lemmas 5.15/5.17 of the whitepaper, the fame of round r
        // witnesses is decided by round r+5, so r+6 is safe.
        let effective_round = round_received + 6;

        let mut changed = false;
        for r in &receipts {
            let tx_body = &r.internal_transaction.body;
            if r.accepted {
                self.logger
                    .with_field("round_received", round_received)
                    .with_field("type", tx_body.typ.to_string())
                    .debug("Processing accepted InternalTransaction");

                match tx_body.typ {
                    TransactionType::PeerAdd => {
                        validators = Arc::new(validators.with_new_peer(&tx_body.peer));
                        current_peers = Arc::new(current_peers.with_new_peer(&tx_body.peer));
                    }
                    TransactionType::PeerRemove => {
                        validators = Arc::new(validators.with_removed_peer(&tx_body.peer));
                        current_peers = Arc::new(current_peers.with_removed_peer(&tx_body.peer));
                        if tx_body.peer.id() == self.validator.id() {
                            self.logger.debug(format!(
                                "Update RemovedRound from {} to {}",
                                self.removed_round, effective_round
                            ));
                            self.removed_round = effective_round;
                        }
                    }
                }
                changed = true;
            } else {
                self.logger.debug("InternalTransaction not accepted");
            }
        }

        if changed {
            self.last_peer_change_round = effective_round;
            self.hg
                .store
                .set_peer_set(effective_round, validators.clone())
                .map_err(|e| anyhow!("Updating Store PeerSet: {}", e))?;
            self.validators = validators;
            self.logger
                .with_field("effective_round", effective_round)
                .with_field("validators", self.validators.peers.len())
                .info("Validators changed");
            self.set_peers(current_peers);
            if effective_round > self.target_round {
                self.logger.debug(format!(
                    "Update TargetRound from {} to {}",
                    self.target_round, effective_round
                ));
                self.target_round = effective_round;
            }
        }

        for r in &receipts {
            let key = r.internal_transaction.hash_string();
            if let Some(p) = self.promises.get(&key) {
                if r.accepted {
                    p.respond(true, effective_round, self.validators.peers.clone());
                } else {
                    p.respond(false, 0, Vec::new());
                }
                self.promises.remove(&key);
            }
        }
        Ok(())
    }

    /// Returns events we know that another peer (per `other_known`) does not,
    /// in topological order.
    pub fn event_diff(&mut self, other_known: HashMap<u32, i64>) -> Result<Vec<Event>> {
        let mut unknown: Vec<Event> = Vec::new();
        let myknown = self.known_events();

        for id in myknown.keys() {
            let ct = other_known.get(id).copied().unwrap_or(-1);
            let peer = match self.hg.store.repertoire_by_id().get(id) {
                Some(p) => p.clone(),
                None => continue,
            };
            let participant_events = self
                .hg
                .store
                .participant_events(&peer.pub_key_string(), ct)?;
            for e in participant_events {
                unknown.push(self.hg.store.get_event(&e)?);
            }
        }

        hg::sort_by_topological_order(&mut unknown);
        Ok(unknown)
    }

    /// Converts wire events to hashgraph events.
    pub fn from_wire(&self, wire_events: Vec<WireEvent>) -> Result<Vec<Event>> {
        wire_events
            .iter()
            .map(|w| self.hg.read_wire_info(w))
            .collect()
    }

    /// Converts hashgraph events to wire events.
    pub fn to_wire(&self, events: &[Event]) -> Vec<WireEvent> {
        events.iter().map(|e| e.to_wire()).collect()
    }

    /// Calls hashgraph `ProcessSigPool`.
    pub fn process_sig_pool(&mut self) -> Result<()> {
        self.hg.process_sig_pool()
    }

    /// Appends transactions to the transaction pool.
    pub fn add_transactions(&mut self, txs: Vec<Vec<u8>>) {
        self.transaction_pool.extend(txs);
    }

    /// Adds an `InternalTransaction` to the pool with a corresponding promise,
    /// and returns a receiver for the promise's response.
    pub fn add_internal_transaction(
        &mut self,
        tx: InternalTransaction,
    ) -> Receiver<JoinPromiseResponse> {
        let promise = JoinPromise::new(tx.clone());
        let rx = promise.resp_rx.clone();
        self.promises.insert(tx.hash_string(), promise);
        self.internal_transaction_pool.push(tx);
        rx
    }

    /// Returns the head event from the store.
    pub fn get_head(&self) -> Result<Event> {
        self.hg.store.get_event(&self.head)
    }

    /// Returns an event from the store.
    pub fn get_event(&self, hash: &str) -> Result<Event> {
        self.hg.store.get_event(hash)
    }

    /// Returns the transactions of an event.
    pub fn get_event_transactions(&self, hash: &str) -> Result<Vec<Vec<u8>>> {
        Ok(self.get_event(hash)?.transactions().to_vec())
    }

    /// Returns the consensus event hashes from the store.
    pub fn get_consensus_events(&self) -> Vec<String> {
        self.hg.store.consensus_events()
    }

    /// Returns the count of consensus events.
    pub fn get_consensus_events_count(&self) -> i64 {
        self.hg.store.consensus_events_count()
    }

    /// Returns the undetermined events from the hashgraph.
    pub fn get_undetermined_events(&self) -> Vec<String> {
        self.hg.undetermined_events.clone()
    }

    /// Returns the number of pending loaded events.
    pub fn get_pending_loaded_events(&self) -> i64 {
        self.hg.pending_loaded_events
    }

    /// Returns the transactions of all consensus events.
    pub fn get_consensus_transactions(&self) -> Result<Vec<Vec<u8>>> {
        let mut txs: Vec<Vec<u8>> = Vec::new();
        for e in self.get_consensus_events() {
            let e_txs = self
                .get_event_transactions(&e)
                .map_err(|_| anyhow!("Consensus event not found: {}", e))?;
            txs.extend(e_txs);
        }
        Ok(txs)
    }

    /// Returns the last consensus round index.
    pub fn get_last_consensus_round_index(&self) -> Option<i64> {
        self.hg.last_consensus_round
    }

    /// Returns the count of consensus transactions.
    pub fn get_consensus_transactions_count(&self) -> i64 {
        self.hg.consensus_transactions
    }

    /// Returns the number of events in the last committed round.
    pub fn get_last_commited_round_events_count(&self) -> i64 {
        self.hg.last_commited_round_events
    }

    /// Returns the last block index from the store.
    pub fn get_last_block_index(&self) -> i64 {
        self.hg.store.last_block_index()
    }
}
