//! Translation of `chain/hashgraph/hashgraph.go` — the Babble distributed
//! consensus algorithm.
//!
//! The `Hashgraph` is accessed exclusively (the Go node serialised every call
//! through `coreLock`), so every method here takes `&mut self`: the six
//! memoization caches mutate on read (LRU recency) and the consensus methods
//! mutate the DAG.

use std::collections::HashMap;

use anyhow::{Result, anyhow};

use super::block::Block;
use super::caches::{Key, PendingRound, PendingRoundsCache, SigPool, TreKey};
use super::event::{
    Event, EventBody, EventCoordinates, FrameEvent, WireEvent, new_coordinates_map,
    sort_frame_events,
};
use super::frame::Frame;
use super::rocks_store::RocksDbStore;
use super::root::Root;
use super::round_info::RoundInfo;
use super::store::Store;
use crate::common::{self, LRU, StoreErrType, is_store};
use crate::logrus::Entry;
use crate::peers::PeerSet;

/// Determines how many `FrameEvent`s are included in a Root. It is deliberately
/// not configurable: peers using different values would produce different
/// Roots, Frames and Blocks.
pub const ROOT_DEPTH: i64 = 10;

/// The frequency of coin rounds.
pub const COIN_ROUND_FREQ: f64 = 4.0;

/// Called by the Hashgraph to commit a Block. A layer of indirection over the
/// proxy commit callback: processing the response is the `Core`'s job.
pub type InternalCommitCallback = Box<dyn Fn(&Block) -> Result<()> + Send>;

/// A no-op commit callback used for testing.
pub fn dummy_internal_commit_callback(_b: &Block) -> Result<()> {
    Ok(())
}

/// A DAG of Events with methods to extract a consensus order and map it onto a
/// blockchain.
pub struct Hashgraph {
    /// Store of Events, Rounds and Blocks.
    pub store: Box<dyn Store>,
    /// FIFO queue of Events whose consensus order is not yet determined.
    pub undetermined_events: Vec<String>,
    /// FIFO queue of Rounds which have not attained consensus yet.
    pub pending_rounds: PendingRoundsCache,
    /// Pool of Block signatures that need to be matched with Blocks.
    pub pending_signatures: SigPool,
    /// Index of the last consensus round.
    pub last_consensus_round: Option<i64>,
    /// Index of the first consensus round (only used in tests).
    pub first_consensus_round: Option<i64>,
    /// Index of the last block with enough signatures.
    pub anchor_block: Option<i64>,
    /// Rounds and events below this lower bound get special treatment.
    round_lower_bound: Option<i64>,
    /// Number of events in the round before `last_consensus_round`.
    pub last_commited_round_events: i64,
    /// Number of consensus transactions.
    pub consensus_transactions: i64,
    /// Number of loaded events that are not yet committed.
    pub pending_loaded_events: i64,
    /// Commit block callback.
    commit_callback: InternalCommitCallback,
    /// Counter used to order events in topological order (node-local).
    topological_index: i64,

    ancestor_cache: LRU<Key, bool>,
    self_ancestor_cache: LRU<Key, bool>,
    strongly_see_cache: LRU<TreKey, bool>,
    round_cache: LRU<String, i64>,
    timestamp_cache: LRU<String, i64>,
    witness_cache: LRU<String, bool>,

    logger: Entry,
}

fn set_vote(votes: &mut HashMap<String, HashMap<String, bool>>, x: &str, y: &str, vote: bool) {
    votes
        .entry(x.to_string())
        .or_default()
        .insert(y.to_string(), vote);
}

fn get_vote(votes: &HashMap<String, HashMap<String, bool>>, x: &str, y: &str) -> bool {
    votes
        .get(x)
        .and_then(|m| m.get(y))
        .copied()
        .unwrap_or(false)
}

impl Hashgraph {
    /// Instantiates a Hashgraph with an underlying data store and a commit
    /// callback.
    pub fn new(
        store: Box<dyn Store>,
        commit_callback: InternalCommitCallback,
        logger: Option<Entry>,
    ) -> Hashgraph {
        let logger = logger.unwrap_or_else(|| {
            let entry = Entry::standalone();
            entry.logger().set_level(crate::logrus::Level::Debug);
            entry
        });

        let cs = store.cache_size().max(0) as usize;
        Hashgraph {
            store,
            undetermined_events: Vec::new(),
            pending_rounds: PendingRoundsCache::new(),
            pending_signatures: SigPool::new(),
            last_consensus_round: None,
            first_consensus_round: None,
            anchor_block: None,
            round_lower_bound: None,
            last_commited_round_events: 0,
            consensus_transactions: 0,
            pending_loaded_events: 0,
            commit_callback,
            topological_index: 0,
            ancestor_cache: LRU::new(cs, None),
            self_ancestor_cache: LRU::new(cs, None),
            strongly_see_cache: LRU::new(cs, None),
            round_cache: LRU::new(cs, None),
            timestamp_cache: LRU::new(cs, None),
            witness_cache: LRU::new(cs, None),
            logger,
        }
    }

    /// Sets the initial PeerSet, which also creates the corresponding Roots and
    /// updates the Repertoire.
    pub fn init(&mut self, peer_set: std::sync::Arc<PeerSet>) -> Result<()> {
        self.store
            .set_peer_set(0, peer_set)
            .map_err(|e| anyhow!("Error setting PeerSet: {}", e))
    }

    // -----------------------------------------------------------------------
    // Private methods
    // -----------------------------------------------------------------------

    /// True if `y` is an ancestor of `x`.
    fn ancestor(&mut self, x: &str, y: &str) -> Result<bool> {
        let k = Key::new(x, y);
        if let Some(c) = self.ancestor_cache.get(&k) {
            return Ok(c);
        }
        let a = self._ancestor(x, y)?;
        self.ancestor_cache.add(k, a);
        Ok(a)
    }

    fn _ancestor(&mut self, x: &str, y: &str) -> Result<bool> {
        if x == y {
            return Ok(true);
        }
        let ex = self.store.get_event(x)?;
        let ey = self.store.get_event(y)?;
        let res = match ex.last_ancestors.get(&ey.creator()) {
            Some(entry) => entry.index >= ey.index(),
            None => false,
        };
        Ok(res)
    }

    /// True if `y` is a self-ancestor of `x`.
    fn self_ancestor(&mut self, x: &str, y: &str) -> Result<bool> {
        let k = Key::new(x, y);
        if let Some(c) = self.self_ancestor_cache.get(&k) {
            return Ok(c);
        }
        let a = self._self_ancestor(x, y)?;
        self.self_ancestor_cache.add(k, a);
        Ok(a)
    }

    fn _self_ancestor(&mut self, x: &str, y: &str) -> Result<bool> {
        if x == y {
            return Ok(true);
        }
        let ex = self.store.get_event(x)?;
        let ey = self.store.get_event(y)?;
        Ok(ex.creator() == ey.creator() && ex.index() >= ey.index())
    }

    /// True if `x` sees `y`.
    fn see(&mut self, x: &str, y: &str) -> Result<bool> {
        self.ancestor(x, y)
    }

    /// True if `x` strongly sees `y` based on the given peer-set.
    fn strongly_see(&mut self, x: &str, y: &str, peers: &PeerSet) -> Result<bool> {
        let k = TreKey::new(x, y, &peers.hex());
        if let Some(c) = self.strongly_see_cache.get(&k) {
            return Ok(c);
        }
        let ss = self._strongly_see(x, y, peers)?;
        self.strongly_see_cache.add(k, ss);
        Ok(ss)
    }

    fn _strongly_see(&mut self, x: &str, y: &str, peers: &PeerSet) -> Result<bool> {
        let ex = self.store.get_event(x)?;
        let ey = self.store.get_event(y)?;

        let mut c: i64 = 0;
        for p in peers.by_pub_key.keys() {
            let xla = ex.last_ancestors.get(p);
            let yfd = ey.first_descendants.get(p);
            if let (Some(xla), Some(yfd)) = (xla, yfd) {
                if xla.index >= yfd.index {
                    c += 1;
                }
            }
        }
        Ok(c >= peers.super_majority())
    }

    fn round(&mut self, x: &str) -> Result<i64> {
        if let Some(c) = self.round_cache.get(&x.to_string()) {
            return Ok(c);
        }
        let r = self._round(x)?;
        self.round_cache.add(x.to_string(), r);
        Ok(r)
    }

    fn _round(&mut self, x: &str) -> Result<i64> {
        let ex = self.store.get_event(x)?;

        let mut parent_round: i64 = -1;

        if !ex.self_parent().is_empty() {
            parent_round = self.round(&ex.self_parent())?;
        }

        if !ex.other_parent().is_empty() {
            let op_round = self.round(&ex.other_parent())?;
            if op_round > parent_round {
                parent_round = op_round;
            }
        }

        if parent_round == -1 {
            return Ok(0);
        }

        let mut round = parent_round;

        // Retrieve the parent round's PeerSet and count strongly-seen witnesses.
        let parent_round_obj = self.store.get_round(parent_round)?;
        let parent_round_peer_set = self.store.get_peer_set(parent_round)?;

        let mut c: i64 = 0;
        for w in parent_round_obj.witnesses() {
            if self.strongly_see(x, &w, &parent_round_peer_set)? {
                c += 1;
            }
        }

        if c >= parent_round_peer_set.super_majority() {
            round += 1;
        }

        Ok(round)
    }

    fn witness(&mut self, x: &str) -> Result<bool> {
        if let Some(c) = self.witness_cache.get(&x.to_string()) {
            return Ok(c);
        }
        let r = self._witness(x)?;
        self.witness_cache.add(x.to_string(), r);
        Ok(r)
    }

    /// True if `x` is a witness (first event of a round for its creator).
    fn _witness(&mut self, x: &str) -> Result<bool> {
        let ex = self.store.get_event(x)?;
        let x_round = self.round(x)?;

        // does the creator belong to the PeerSet?
        let peer_set = self.store.get_peer_set(x_round)?;
        if !peer_set.by_pub_key.contains_key(&ex.creator()) {
            return Ok(false);
        }

        let mut sp_round: i64 = -1;
        if !ex.self_parent().is_empty() {
            sp_round = self.round(&ex.self_parent())?;
        }

        Ok(x_round > sp_round)
    }

    fn round_received(&mut self, x: &str) -> Result<i64> {
        let ex = self.store.get_event(x)?;
        Ok(ex.round_received.unwrap_or(-1))
    }

    fn lamport_timestamp(&mut self, x: &str) -> Result<i64> {
        if let Some(c) = self.timestamp_cache.get(&x.to_string()) {
            return Ok(c);
        }
        let r = self._lamport_timestamp(x)?;
        self.timestamp_cache.add(x.to_string(), r);
        Ok(r)
    }

    fn _lamport_timestamp(&mut self, x: &str) -> Result<i64> {
        let mut plt: i64 = -1;
        let ex = self.store.get_event(x)?;

        if !ex.self_parent().is_empty() {
            plt = self.lamport_timestamp(&ex.self_parent())?;
        }

        if !ex.other_parent().is_empty() {
            let mut op_lt = i32::MIN as i64;
            if self.store.get_event(&ex.other_parent()).is_ok() {
                op_lt = self.lamport_timestamp(&ex.other_parent())?;
            }
            if op_lt > plt {
                plt = op_lt;
            }
        }

        Ok(plt + 1)
    }

    /// `round(x) - round(y)`.
    fn round_diff(&mut self, x: &str, y: &str) -> Result<i64> {
        let x_round = self
            .round(x)
            .map_err(|_| anyhow!("event {} has negative round", x))?;
        let y_round = self
            .round(y)
            .map_err(|_| anyhow!("event {} has negative round", y))?;
        Ok(x_round - y_round)
    }

    /// Checks the self-parent is the creator's last known event.
    fn check_self_parent(&self, event: &Event) -> Result<()> {
        let self_parent = event.self_parent();
        let creator = event.creator();

        match self.store.last_event_from(&creator) {
            Ok(creator_last_known) => {
                let self_parent_legit = self_parent == creator_last_known;
                if !self_parent_legit {
                    return Err(super::errors::SelfParentError::new(
                        "Self-parent not last known event by creator",
                        true,
                    )
                    .into());
                }
                Ok(())
            }
            Err(e) => {
                // First event.
                if is_store(&e, StoreErrType::Empty) && self_parent.is_empty() {
                    return Ok(());
                }
                Err(super::errors::SelfParentError::new(&e.to_string(), false).into())
            }
        }
    }

    /// Checks the other-parent is known.
    fn check_other_parent(&self, event: &Event) -> Result<()> {
        let other_parent = event.other_parent();
        if !other_parent.is_empty() && self.store.get_event(&other_parent).is_err() {
            return Err(anyhow!("Other-parent not known"));
        }
        Ok(())
    }

    /// Initializes the arrays of last-ancestors and first-descendants.
    fn init_event_coordinates(&mut self, event: &mut Event) -> Result<()> {
        event.last_ancestors = new_coordinates_map();
        event.first_descendants = new_coordinates_map();

        let self_parent = self.store.get_event(&event.self_parent());
        let other_parent = self.store.get_event(&event.other_parent());

        match (&self_parent, &other_parent) {
            (Err(_), Ok(op)) => {
                event.last_ancestors = op.last_ancestors.clone();
            }
            (Ok(sp), Err(_)) => {
                event.last_ancestors = sp.last_ancestors.clone();
            }
            (Ok(sp), Ok(op)) => {
                event.last_ancestors = sp.last_ancestors.clone();
                for (p, ola) in &op.last_ancestors {
                    match event.last_ancestors.get(p) {
                        Some(sla) if sla.index >= ola.index => {}
                        _ => {
                            event.last_ancestors.insert(
                                p.clone(),
                                EventCoordinates {
                                    index: ola.index,
                                    hash: ola.hash.clone(),
                                },
                            );
                        }
                    }
                }
            }
            (Err(_), Err(_)) => {}
        }

        event.first_descendants.insert(
            event.creator(),
            EventCoordinates {
                index: event.index(),
                hash: event.hex(),
            },
        );
        event.last_ancestors.insert(
            event.creator(),
            EventCoordinates {
                index: event.index(),
                hash: event.hex(),
            },
        );

        Ok(())
    }

    /// Updates the first-descendant of each ancestor of `event`.
    fn update_ancestor_first_descendant(&mut self, event: &Event) -> Result<()> {
        let creator = event.creator();
        let index = event.index();
        let hex = event.hex();

        let ancestor_hashes: Vec<String> = event
            .last_ancestors
            .values()
            .map(|c| c.hash.clone())
            .collect();

        for mut ah in ancestor_hashes {
            loop {
                let mut a = match self.store.get_event(&ah) {
                    Ok(a) => a,
                    Err(_) => break,
                };

                if a.first_descendants.contains_key(&creator) {
                    break;
                }

                a.first_descendants.insert(
                    creator.clone(),
                    EventCoordinates {
                        index,
                        hash: hex.clone(),
                    },
                );
                self.store.set_event(&a)?;

                // Stop at the ancestors that are witnesses.
                if let Ok(true) = self.witness(&ah) {
                    break;
                }
                ah = a.self_parent();
            }
        }
        Ok(())
    }

    fn create_frame_event(&mut self, x: &str) -> Result<FrameEvent> {
        let ev = self
            .store
            .get_event(x)
            .map_err(|_| anyhow!("FrameEvent {} not found", x))?;

        let round = self.round(x)?;
        let round_info = self.store.get_round(round)?;

        let te = round_info
            .created_events
            .get(x)
            .copied()
            .ok_or_else(|| anyhow!("round {} CreatedEvents[{}] not found", round, x))?;

        let lt = self.lamport_timestamp(x)?;

        Ok(FrameEvent {
            core: Box::new(ev),
            round,
            lamport_timestamp: lt,
            witness: te.witness,
        })
    }

    fn create_root(&mut self, participant: &str, head: &str) -> Result<Root> {
        let mut root = Root::new();

        if !head.is_empty() {
            let head_event = self.create_frame_event(head)?;
            let mut reverse_root_events: Vec<FrameEvent> = vec![head_event.clone()];

            let mut index = head_event.core.index();
            for _ in 0..ROOT_DEPTH {
                index -= 1;
                if index >= 0 {
                    match self.store.participant_event(participant, index) {
                        Ok(peh) => {
                            let rev = self.create_frame_event(&peh)?;
                            reverse_root_events.push(rev);
                        }
                        Err(_) => break,
                    }
                } else {
                    break;
                }
            }

            for rev in reverse_root_events.into_iter().rev() {
                root.insert(rev);
            }
        }

        Ok(root)
    }

    /// Sets the private wire-info fields on `event`.
    pub fn set_wire_info(&self, event: &mut Event) -> Result<()> {
        let mut self_parent_index: i64 = -1;
        let mut other_parent_creator_id: u32 = 0;
        let mut other_parent_index: i64 = -1;

        let repertoire = self.store.repertoire_by_pub_key();
        let creator = repertoire
            .get(&event.creator())
            .ok_or_else(|| anyhow!("Creator {} not found", event.creator()))?;

        if !event.self_parent().is_empty() {
            let self_parent = self.store.get_event(&event.self_parent())?;
            self_parent_index = self_parent.index();
        }

        if !event.other_parent().is_empty() {
            let other_parent = self.store.get_event(&event.other_parent())?;
            let other_parent_creator = repertoire
                .get(&other_parent.creator())
                .ok_or_else(|| anyhow!("Creator {} not found", other_parent.creator()))?;
            other_parent_creator_id = other_parent_creator.id();
            other_parent_index = other_parent.index();
        }

        event.set_wire_info(
            self_parent_index,
            other_parent_creator_id,
            other_parent_index,
            creator.id(),
        );
        Ok(())
    }

    /// Removes processed signatures from the SigPool.
    fn remove_processed_signatures(&mut self, processed_signatures: &HashMap<String, bool>) {
        for k in processed_signatures.keys() {
            self.pending_signatures.remove(k);
        }
    }

    // -----------------------------------------------------------------------
    // Public consensus methods
    // -----------------------------------------------------------------------

    /// Inserts an Event and runs the consensus methods.
    pub fn insert_event_and_run_consensus(
        &mut self,
        event: &mut Event,
        set_wire_info: bool,
    ) -> Result<()> {
        if let Err(e) = self.insert_event(event, set_wire_info) {
            if !super::errors::is_normal_self_parent_error(&e) {
                self.logger.with_error(&e).error("InsertEvent");
            }
            return Err(e);
        }
        if let Err(e) = self.divide_rounds() {
            self.logger.with_error(&e).error("DivideRounds");
            return Err(e);
        }
        if let Err(e) = self.decide_fame() {
            self.logger.with_error(&e).error("DecideFame");
            return Err(e);
        }
        if let Err(e) = self.decide_round_received() {
            self.logger.with_error(&e).error("DecideRoundReceived");
            return Err(e);
        }
        if let Err(e) = self.process_decided_rounds() {
            self.logger.with_error(&e).error("ProcessDecidedRounds");
            return Err(e);
        }
        Ok(())
    }

    /// Attempts to insert an Event in the DAG. Verifies the signature, checks
    /// the ancestors are known and prevents the introduction of forks.
    pub fn insert_event(&mut self, event: &mut Event, set_wire_info: bool) -> Result<()> {
        // verify signature
        match event.verify() {
            Ok(true) => {}
            Ok(false) => {
                self.logger
                    .with_field("event", event.hex())
                    .with_field("creator", event.creator())
                    .with_field("self_parent", event.self_parent())
                    .error("Invalid Event signature");
                return Err(anyhow!("Invalid Event signature {}", event.hex()));
            }
            Err(e) => return Err(e),
        }

        if let Err(e) = self.check_self_parent(event) {
            let entry = self
                .logger
                .with_field("event", event.hex())
                .with_field("creator", event.creator())
                .with_field("self_parent", event.self_parent())
                .with_error(&e);
            if !super::errors::is_normal_self_parent_error(&e) {
                entry.error("CheckSelfParent");
            } else {
                entry.trace("CheckSelfParent");
            }
            return Err(e);
        }

        if let Err(e) = self.check_other_parent(event) {
            self.logger
                .with_field("event", event.hex())
                .with_field("creator", event.creator())
                .with_field("other_parent", event.other_parent())
                .with_error(&e)
                .error("CheckOtherParent");
            return Err(e);
        }

        event.topological_index = self.topological_index;
        self.topological_index += 1;

        if set_wire_info {
            self.set_wire_info(event)
                .map_err(|e| anyhow!("SetWireInfo: {}", e))?;
        }

        self.init_event_coordinates(event)
            .map_err(|e| anyhow!("InitEventCoordinates: {}", e))?;

        self.store
            .set_event(event)
            .map_err(|e| anyhow!("SetEvent: {}", e))?;

        self.update_ancestor_first_descendant(event)
            .map_err(|e| anyhow!("UpdateAncestorFirstDescendant: {}", e))?;

        self.undetermined_events.push(event.hex());

        if event.is_loaded() {
            self.pending_loaded_events += 1;
        }

        for bs in event.block_signatures() {
            self.logger
                .debug(format!("Inserting pending signature {}", bs.key()));
            self.pending_signatures.add(bs.clone());
        }

        Ok(())
    }

    /// Inserts the FrameEvent's core Event without checking parents or
    /// signature, and without adding it to `undetermined_events`.
    pub fn insert_frame_event(&mut self, frame_event: &FrameEvent) -> Result<()> {
        let mut event = (*frame_event.core).clone();

        // Set caches so round, witness and timestamp won't be recalculated.
        self.round_cache.add(event.hex(), frame_event.round);
        self.witness_cache.add(event.hex(), frame_event.witness);
        self.timestamp_cache
            .add(event.hex(), frame_event.lamport_timestamp);

        // Set the event's private fields for later use.
        event.set_round(frame_event.round);
        event.set_lamport_timestamp(frame_event.lamport_timestamp);

        // Create/update the RoundInfo object in the store.
        let mut round_info = match self.store.get_round(frame_event.round) {
            Ok(ri) => ri,
            Err(e) => {
                if !is_store(&e, StoreErrType::KeyNotFound) {
                    return Err(e);
                }
                RoundInfo::new()
            }
        };
        round_info.add_created_event(&event.hex(), frame_event.witness);
        self.store.set_round(frame_event.round, &round_info)?;

        self.init_event_coordinates(&mut event)
            .map_err(|e| anyhow!("InitEventCoordinates: {}", e))?;

        self.store
            .set_event(&event)
            .map_err(|e| anyhow!("SetEvent: {}", e))?;

        self.update_ancestor_first_descendant(&event)
            .map_err(|e| anyhow!("UpdateAncestorFirstDescendant: {}", e))?;

        self.store
            .add_consensus_event(&event)
            .map_err(|_| anyhow!("AddConsensusEvent"))?;

        Ok(())
    }

    /// Assigns a Round and LamportTimestamp to Events, flags witnesses, and
    /// pushes Rounds into the PendingRounds queue if necessary.
    pub fn divide_rounds(&mut self) -> Result<()> {
        let undetermined = self.undetermined_events.clone();
        for hash in undetermined {
            let mut ev = self.store.get_event(&hash)?;
            let mut update_event = false;

            if ev.round.is_none() {
                let round_number = self.round(&hash)?;
                ev.set_round(round_number);
                update_event = true;

                let mut round_info = match self.store.get_round(round_number) {
                    Ok(ri) => ri,
                    Err(e) => {
                        if !is_store(&e, StoreErrType::KeyNotFound) {
                            return Err(e);
                        }
                        RoundInfo::new()
                    }
                };

                if !self.pending_rounds.queued(round_number)
                    && !round_info.decided
                    && (self.round_lower_bound.is_none()
                        || round_number > self.round_lower_bound.unwrap())
                {
                    self.pending_rounds.set(PendingRound {
                        index: round_number,
                        decided: false,
                    });
                }

                let witness = self.witness(&hash)?;
                round_info.add_created_event(&hash, witness);
                self.store.set_round(round_number, &round_info)?;
            }

            if ev.lamport_timestamp.is_none() {
                let lamport_timestamp = self.lamport_timestamp(&hash)?;
                ev.set_lamport_timestamp(lamport_timestamp);
                update_event = true;
            }

            if update_event {
                let _ = self.store.set_event(&ev);
            }
        }
        Ok(())
    }

    /// Decides if witnesses are famous.
    pub fn decide_fame(&mut self) -> Result<()> {
        let mut votes: HashMap<String, HashMap<String, bool>> = HashMap::new();
        let mut decided_rounds: Vec<i64> = Vec::new();

        for r in self.pending_rounds.get_ordered_pending_rounds() {
            let round_index = r.index;

            let mut r_round_info = self.store.get_round(round_index)?;
            let r_peer_set = self.store.get_peer_set(round_index)?;

            let r_witnesses = r_round_info.witnesses();
            for x in &r_witnesses {
                if r_round_info.is_decided(x) {
                    continue;
                }
                let last_round = self.store.last_round();
                'vote_loop: for j in (round_index + 1)..=last_round {
                    let j_round_info = self.store.get_round(j)?;
                    let j_peer_set = self.store.get_peer_set(j)?;

                    for y in j_round_info.witnesses() {
                        let diff = j - round_index;
                        if diff == 1 {
                            let ycx = self.see(&y, x)?;
                            set_vote(&mut votes, &y, x, ycx);
                        } else {
                            let j_prev_round_info = self.store.get_round(j - 1)?;
                            let j_prev_peer_set = self.store.get_peer_set(j - 1)?;

                            let mut ss_witnesses: Vec<String> = Vec::new();
                            for w in j_prev_round_info.witnesses() {
                                if self.strongly_see(&y, &w, &j_prev_peer_set)? {
                                    ss_witnesses.push(w);
                                }
                            }

                            let mut yays = 0i64;
                            let mut nays = 0i64;
                            for w in &ss_witnesses {
                                if get_vote(&votes, w, x) {
                                    yays += 1;
                                } else {
                                    nays += 1;
                                }
                            }
                            let mut v = false;
                            let mut t = nays;
                            if yays >= nays {
                                v = true;
                                t = yays;
                            }

                            if (diff as f64) % COIN_ROUND_FREQ > 0.0 {
                                // normal round
                                if t >= j_peer_set.super_majority() {
                                    r_round_info.set_fame(x, v);
                                    set_vote(&mut votes, &y, x, v);
                                    break 'vote_loop;
                                } else {
                                    set_vote(&mut votes, &y, x, v);
                                }
                            } else {
                                // coin round
                                if t >= j_peer_set.super_majority() {
                                    set_vote(&mut votes, &y, x, v);
                                } else {
                                    set_vote(&mut votes, &y, x, middle_bit(&y));
                                }
                            }
                        }
                    }
                }
            }

            if r_round_info.witnesses_decided(&r_peer_set) {
                decided_rounds.push(round_index);
            }
            self.store.set_round(round_index, &r_round_info)?;
        }

        self.pending_rounds.update(&decided_rounds);
        Ok(())
    }

    /// Assigns a RoundReceived to undetermined events when they reach
    /// consensus.
    pub fn decide_round_received(&mut self) -> Result<()> {
        let mut new_undetermined_events: Vec<String> = Vec::new();
        let undetermined = self.undetermined_events.clone();

        for x in undetermined {
            let mut received = false;
            let r = self.round(&x)?;
            let last_round = self.store.last_round();

            for i in (r + 1)..=last_round {
                let mut tr = match self.store.get_round(i) {
                    Ok(tr) => tr,
                    Err(_) => break,
                };
                let t_peers = self.store.get_peer_set(i)?;

                if !tr.witnesses_decided(&t_peers) {
                    if self.round_lower_bound.is_none() || self.round_lower_bound.unwrap() < i {
                        break;
                    } else {
                        continue;
                    }
                }

                let fws = tr.famous_witnesses();
                let mut s: Vec<String> = Vec::new();
                for w in &fws {
                    if self.see(w, &x)? {
                        s.push(w.clone());
                    }
                }

                if s.len() == fws.len() && s.len() as i64 >= t_peers.super_majority() {
                    received = true;

                    let mut ex = self.store.get_event(&x)?;
                    ex.set_round_received(i);
                    self.store.set_event(&ex)?;

                    tr.add_received_event(&x);
                    self.store.set_round(i, &tr)?;
                    break;
                }
            }

            if !received {
                new_undetermined_events.push(x);
            }
        }

        self.undetermined_events = new_undetermined_events;
        Ok(())
    }

    /// Takes Rounds whose witnesses are decided, computes Frames, maps them
    /// into Blocks and commits the Blocks via the commit callback.
    pub fn process_decided_rounds(&mut self) -> Result<()> {
        let mut processed_rounds: Vec<i64> = Vec::new();
        let result = self.process_decided_rounds_inner(&mut processed_rounds);
        self.pending_rounds.clean(&processed_rounds);
        result
    }

    fn process_decided_rounds_inner(&mut self, processed_rounds: &mut Vec<i64>) -> Result<()> {
        for r in self.pending_rounds.get_ordered_pending_rounds() {
            // Never process a decided round before all earlier rounds.
            if !r.decided {
                break;
            }

            let round = self.store.get_round(r.index)?;
            let frame = self
                .get_frame(r.index)
                .map_err(|e| anyhow!("Getting Frame {}: {}", r.index, e))?;

            self.logger
                .with_field("round_received", r.index)
                .with_field("witnesses", round.famous_witnesses().len())
                .with_field("created_events", round.created_events.len())
                .with_field("events", frame.events.len())
                .with_field("peers", frame.peers.len())
                .debug("Processing Decided Round");

            if !frame.events.is_empty() {
                for e in &frame.events {
                    self.store.add_consensus_event(&e.core)?;
                    self.consensus_transactions += e.core.transactions().len() as i64;
                    if e.core.is_loaded() {
                        self.pending_loaded_events -= 1;
                    }
                }

                let last_block_index = self.store.last_block_index();
                let block = Block::new_from_frame(last_block_index + 1, &frame)?;

                if !block.transactions().is_empty() || !block.internal_transactions().is_empty() {
                    self.store.set_block(&block)?;
                    if (self.commit_callback)(&block).is_err() {
                        self.logger
                            .warn(format!("Failed to commit block {}", block.index()));
                    }
                }

                self.last_commited_round_events = frame.events.len() as i64;
            } else {
                self.logger.debug(format!(
                    "No Events to commit for ConsensusRound {}",
                    r.index
                ));
            }

            processed_rounds.push(r.index);

            if self.last_consensus_round.is_none() || r.index > self.last_consensus_round.unwrap() {
                self.set_last_consensus_round(r.index);
            }
        }
        Ok(())
    }

    /// Computes the Frame corresponding to a round-received.
    pub fn get_frame(&mut self, round_received: i64) -> Result<Frame> {
        // Try the store first.
        match self.store.get_frame(round_received) {
            Ok(frame) => return Ok(frame),
            Err(e) => {
                if !is_store(&e, StoreErrType::KeyNotFound) {
                    return Err(e);
                }
            }
        }

        let round = self.store.get_round(round_received)?;
        let peer_set = self.store.get_peer_set(round_received)?;

        let mut events: Vec<FrameEvent> = Vec::new();
        for eh in &round.received_events {
            events.push(self.create_frame_event(eh)?);
        }
        sort_frame_events(&mut events);

        // Get/create Roots. Events are in topological order, so the first
        // event of a participant triggers Root creation.
        let mut roots: std::collections::BTreeMap<String, Root> = std::collections::BTreeMap::new();
        for ev in &events {
            let p = ev.core.creator();
            if !roots.contains_key(&p) {
                let sp = ev.core.self_parent();
                let r = self.create_root(&p, &sp)?;
                roots.insert(p, r);
            }
        }

        // Every participant known before round_received needs a Root.
        let repertoire = self.store.repertoire_by_pub_key();
        for (p, peer) in &repertoire {
            let (first_round, ok) = self.store.first_round(peer.id());
            if !ok || first_round > round_received {
                continue;
            }
            if !roots.contains_key(p) {
                let last_consensus_event_hash = self.store.last_consensus_event_from(p)?;
                let root = self.create_root(p, &last_consensus_event_hash)?;
                roots.insert(p.clone(), root);
            }
        }

        let all_peer_sets = self.store.get_all_peer_sets()?;
        let peer_sets: std::collections::BTreeMap<i64, Vec<_>> =
            all_peer_sets.into_iter().collect();

        // Compute the BFT timestamp.
        let mut timestamps: Vec<i64> = Vec::new();
        for fw in round.famous_witnesses() {
            let ev = self.store.get_event(&fw)?;
            timestamps.push(ev.timestamp());
        }
        let frame_timestamp = common::median(&timestamps);

        let res = Frame {
            round: round_received,
            peers: peer_set.peers.clone(),
            roots,
            events,
            peer_sets,
            timestamp: frame_timestamp,
        };

        self.store.set_frame(&res)?;
        Ok(res)
    }

    /// Maps pending signatures to known Blocks; valid signatures are appended
    /// and the AnchorBlock is updated if necessary.
    pub fn process_sig_pool(&mut self) -> Result<()> {
        self.logger
            .with_field("pending_signatures", self.pending_signatures.len())
            .debug("ProcessSigPool()");

        for (_k, bs) in self.pending_signatures.items() {
            let mut block = match self.store.get_block(bs.index) {
                Ok(b) => b,
                Err(e) => {
                    self.logger
                        .with_field("index", bs.index)
                        .with_field("msg", e)
                        .warn("Verifying Block signature. Could not fetch Block");
                    continue;
                }
            };

            let peer_set = match self.store.get_peer_set(block.round_received()) {
                Ok(ps) => ps,
                Err(e) => {
                    self.logger
                        .with_field("index", bs.index)
                        .with_field("round", block.round_received())
                        .with_field("err", e)
                        .warn("Verifying Block signature. No PeerSet for Block's Round");
                    continue;
                }
            };

            if !peer_set.by_pub_key.contains_key(&bs.validator_hex()) {
                self.logger
                    .with_field("index", bs.index)
                    .with_field("round", block.round_received())
                    .with_field("validator", bs.validator_hex())
                    .warn(
                        "Verifying Block signature. Validator does not belong to Block's PeerSet",
                    );
                continue;
            }

            let valid = block.verify(bs.clone())?;
            if !valid {
                self.logger
                    .with_field("index", bs.index)
                    .with_field("validator", bs.validator_hex())
                    .warn("Verifying Block signature. Invalid signature");
                continue;
            }

            block.set_signature(bs.clone())?;

            if let Err(e) = self.store.set_block(&block) {
                self.logger
                    .with_field("index", bs.index)
                    .with_field("msg", e)
                    .warn("Saving Block");
            }

            self.set_anchor_block(&block)?;

            self.logger.debug(format!("processed sig {}", bs.key()));
            self.pending_signatures.remove(&bs.key());
        }

        Ok(())
    }

    /// Sets the AnchorBlock index if `block` has collected enough signatures
    /// (+1/3) and is above the current AnchorBlock.
    pub fn set_anchor_block(&mut self, block: &Block) -> Result<()> {
        let peer_set = match self.store.get_peer_set(block.round_received()) {
            Ok(ps) => ps,
            Err(e) => {
                self.logger
                    .with_error(&e)
                    .error("No PeerSet for Block's Round");
                return Err(e);
            }
        };

        if (block.signatures.len() as i64) > peer_set.trust_count()
            && (self.anchor_block.is_none() || block.index() > self.anchor_block.unwrap())
        {
            self.set_anchor_block_index(block.index());
            self.logger
                .with_field("block_index", block.index())
                .with_field("signatures", block.signatures.len())
                .with_field("trustCount", peer_set.trust_count())
                .debug("Setting AnchorBlock");
        } else {
            let msg = match self.anchor_block {
                Some(ab) => ab.to_string(),
                None => "Anchor Block not set".to_string(),
            };
            self.logger
                .with_field("index", block.index())
                .with_field("sigs", block.signatures.len())
                .with_field("trust_count", peer_set.trust_count())
                .with_field("anchor_block", msg)
                .debug("Block is not a suitable Anchor");
        }

        Ok(())
    }

    /// Returns the AnchorBlock and its corresponding Frame.
    pub fn get_anchor_block_with_frame(&mut self) -> Result<(Block, Frame)> {
        let anchor = self
            .anchor_block
            .ok_or_else(|| anyhow!("No Anchor Block"))?;
        let block = self.store.get_block(anchor)?;
        let frame = self.get_frame(block.round_received())?;
        Ok((block, frame))
    }

    /// Clears the Hashgraph and resets it from a new base.
    pub fn reset(&mut self, block: &Block, frame: &Frame) -> Result<()> {
        self.last_consensus_round = None;
        self.first_consensus_round = None;
        self.anchor_block = None;

        self.undetermined_events = Vec::new();
        self.pending_rounds = PendingRoundsCache::new();
        self.pending_loaded_events = 0;
        self.topological_index = 0;

        let cs = self.store.cache_size().max(0) as usize;
        self.ancestor_cache = LRU::new(cs, None);
        self.self_ancestor_cache = LRU::new(cs, None);
        self.strongly_see_cache = LRU::new(cs, None);
        self.round_cache = LRU::new(cs, None);
        self.witness_cache = LRU::new(cs, None);

        self.store.reset(frame)?;

        let sorted_frame_events = frame.sorted_frame_events();
        for rev in &sorted_frame_events {
            self.insert_frame_event(rev)?;
        }

        self.store.set_block(block)?;
        self.set_last_consensus_round(block.round_received());
        self.set_round_lower_bound(block.round_received());

        Ok(())
    }

    /// Loads all Events from the Store's DB and feeds them to the consensus
    /// methods in topological order. WE CAN ONLY BOOTSTRAP FROM 0.
    pub fn bootstrap(&mut self) -> Result<()> {
        // Phase 1: read everything from the persistent DB.
        let mut batches: Vec<Vec<Event>> = Vec::new();
        let mut restore_maintenance = false;
        let mut has_rocks = false;

        {
            if let Some(rocks) = self.store.as_any().downcast_ref::<RocksDbStore>() {
                has_rocks = true;
                if !rocks.get_maintenance_mode() {
                    restore_maintenance = true;
                }
                rocks.set_maintenance_mode(true);

                match rocks.db_get_peer_set(0) {
                    Err(_) => {
                        self.logger.debug("No Genesis PeerSet, skip bootstrap");
                        if restore_maintenance {
                            rocks.set_maintenance_mode(false);
                        }
                        return Ok(());
                    }
                    Ok(peer_set) => {
                        // Initialize the InmemStore with the Genesis PeerSet.
                        rocks.set_peer_set(0, std::sync::Arc::new(peer_set))?;

                        let batch_size = 100i64;
                        let mut index = 0i64;
                        loop {
                            let batch =
                                rocks.db_topological_events(index * batch_size, batch_size)?;
                            let n = batch.len() as i64;
                            batches.push(batch);
                            if n < batch_size {
                                break;
                            }
                            index += 1;
                        }
                    }
                }
            }
        }

        if !has_rocks {
            return Ok(());
        }

        // Phase 2: insert the Events into the Hashgraph.
        for batch in batches {
            for mut e in batch {
                self.insert_event_and_run_consensus(&mut e, true)?;
            }
            self.process_sig_pool()?;
        }

        if restore_maintenance {
            if let Some(rocks) = self.store.as_any().downcast_ref::<RocksDbStore>() {
                rocks.set_maintenance_mode(false);
            }
        }

        Ok(())
    }

    /// Converts a `WireEvent` to an `Event` by replacing int IDs with the
    /// corresponding public keys.
    pub fn read_wire_info(&self, wevent: &WireEvent) -> Result<Event> {
        let mut self_parent = String::new();
        let mut other_parent = String::new();

        let repertoire_by_id = self.store.repertoire_by_id();
        let creator = repertoire_by_id
            .get(&wevent.body.creator_id)
            .ok_or_else(|| anyhow!("Creator {} not found", wevent.body.creator_id))?;

        let creator_bytes = common::decode_from_string(&creator.pub_key_string())?;

        if wevent.body.self_parent_index >= 0 {
            self_parent = self
                .store
                .participant_event(&creator.pub_key_string(), wevent.body.self_parent_index)?;
        }

        if wevent.body.other_parent_index >= 0 {
            let other_parent_creator = repertoire_by_id
                .get(&wevent.body.other_parent_creator_id)
                .ok_or_else(|| {
                anyhow!(
                    "Participant {} not found",
                    wevent.body.other_parent_creator_id
                )
            })?;
            other_parent = self
                .store
                .participant_event(
                    &other_parent_creator.pub_key_string(),
                    wevent.body.other_parent_index,
                )
                .map_err(|_| {
                    anyhow!(
                        "OtherParent (creator: {}, index: {}) not found",
                        wevent.body.other_parent_creator_id,
                        wevent.body.other_parent_index
                    )
                })?;
        }

        let body = EventBody {
            transactions: wevent.body.transactions.clone(),
            internal_transactions: wevent.body.internal_transactions.clone(),
            block_signatures: wevent.block_signatures(&creator_bytes),
            parents: vec![self_parent, other_parent],
            creator: creator_bytes,
            index: wevent.body.index,
            timestamp: wevent.body.timestamp,
            self_parent_index: wevent.body.self_parent_index,
            other_parent_creator_id: wevent.body.other_parent_creator_id,
            other_parent_index: wevent.body.other_parent_index,
            creator_id: wevent.body.creator_id,
        };

        Ok(Event {
            body,
            signature: wevent.signature.clone(),
            ..Event::default()
        })
    }

    /// Returns an error if the Block does not contain valid signatures from
    /// MORE than 1/3 of participants.
    pub fn check_block(&self, block: &Block, peer_set: &PeerSet) -> Result<()> {
        let psh = peer_set.hash();
        if psh != block.peers_hash() {
            return Err(anyhow!("Wrong PeerSet"));
        }

        let mut valid_signatures = 0i64;
        for s in block.get_signatures() {
            let validator_hex = s.validator_hex();
            if !peer_set.by_pub_key.contains_key(&validator_hex) {
                self.logger
                    .with_field("validator", validator_hex)
                    .warn("Verifying Block signature. Unknown validator");
                continue;
            }
            if block.verify(s).unwrap_or(false) {
                valid_signatures += 1;
            }
        }

        if valid_signatures <= peer_set.trust_count() {
            return Err(anyhow!(
                "Not enough valid signatures: got {}, need {}",
                valid_signatures,
                peer_set.trust_count()
            ));
        }

        self.logger
            .with_field("valid_signatures", valid_signatures)
            .debug("CheckBlock");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Setters
    // -----------------------------------------------------------------------

    fn set_last_consensus_round(&mut self, i: i64) {
        self.last_consensus_round = Some(i);
        if self.first_consensus_round.is_none() {
            self.first_consensus_round = Some(i);
        }
    }

    fn set_round_lower_bound(&mut self, i: i64) {
        self.round_lower_bound = Some(i);
    }

    fn set_anchor_block_index(&mut self, i: i64) {
        self.anchor_block = Some(i);
    }
}

/// The middle bit of an event's hash.
fn middle_bit(ehex: &str) -> bool {
    let hash = match common::decode_from_string(ehex) {
        Ok(h) => h,
        Err(e) => {
            println!("ERROR decoding hex string: {}", e);
            Vec::new()
        }
    };
    !(!hash.is_empty() && hash[hash.len() / 2] == 0)
}

#[cfg(test)]
mod tests {
    //! Translation of `chain/hashgraph/hashgraph_test.go`.
    //!
    //! The test module lives inside `hashgraph.rs` so it can exercise the
    //! private predicate methods (`ancestor`, `see`, `lamport_timestamp`, ...).

    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    use k256::ecdsa::SigningKey;

    use super::super::block::BlockSignature;
    use super::super::event::{CoordinatesMap, Event, EventCoordinates};
    use super::super::inmem_store::InmemStore;
    use super::super::rocks_store::RocksDbStore;
    use super::super::round_info::{RoundEvent, RoundInfo};
    use crate::common;
    use crate::common::Trilean;
    use crate::crypto::keys;
    use crate::peers::{Peer, PeerSet};

    const CACHE_SIZE: i64 = 100;
    const N: usize = 3;

    /// Translation of the Go `TestNode` struct.
    struct TestNode {
        pub_bytes: Vec<u8>,
        key: SigningKey,
        #[allow(dead_code)]
        events: Vec<Event>,
    }

    impl TestNode {
        fn new(key: SigningKey) -> TestNode {
            let pub_bytes = keys::from_public_key(key.verifying_key());
            TestNode {
                pub_bytes,
                key,
                events: Vec::new(),
            }
        }

        fn sign_and_add_event(
            &mut self,
            mut event: Event,
            name: &str,
            index: &mut HashMap<String, String>,
            ordered_events: &mut Vec<Event>,
        ) {
            event.sign(&self.key).unwrap();
            index.insert(name.to_string(), event.hex());
            self.events.push(event.clone());
            ordered_events.push(event);
        }
    }

    /// Translation of the Go `play` struct.
    struct Play {
        to: usize,
        index: i64,
        self_parent: String,
        other_parent: String,
        name: String,
        tx_payload: Vec<Vec<u8>>,
        sig_payload: Vec<BlockSignature>,
    }

    /// Convenience constructor for a `Play` with no transaction/signature
    /// payload (the common case in the Go tests).
    fn p(to: usize, index: i64, self_parent: &str, other_parent: &str, name: &str) -> Play {
        Play {
            to,
            index,
            self_parent: self_parent.to_string(),
            other_parent: other_parent.to_string(),
            name: name.to_string(),
            tx_payload: Vec::new(),
            sig_payload: Vec::new(),
        }
    }

    fn idx(index: &HashMap<String, String>, name: &str) -> String {
        index.get(name).cloned().unwrap_or_default()
    }

    fn test_logger() -> Entry {
        common::new_test_entry(common::TEST_LOG_LEVEL)
    }

    fn temp_badger_dir() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!("babble-hg-{}-{}", std::process::id(), nanos))
            .to_string_lossy()
            .into_owned()
    }

    fn init_hashgraph_nodes(
        n: usize,
    ) -> (Vec<TestNode>, HashMap<String, String>, Vec<Event>, PeerSet) {
        let mut nodes = Vec::new();
        let mut pirs = Vec::new();
        for _ in 0..n {
            let key = keys::generate_ecdsa_key().unwrap();
            let pub_hex = keys::public_key_hex(key.verifying_key());
            pirs.push(Peer::new(&pub_hex, "", ""));
            nodes.push(TestNode::new(key));
        }
        let peer_set = PeerSet::new(pirs);
        (nodes, HashMap::new(), Vec::new(), peer_set)
    }

    fn play_events(
        plays: &[Play],
        nodes: &mut [TestNode],
        index: &mut HashMap<String, String>,
        ordered_events: &mut Vec<Event>,
    ) {
        for play in plays {
            let sp = idx(index, &play.self_parent);
            let op = idx(index, &play.other_parent);
            let e = Event::new(
                play.tx_payload.clone(),
                vec![],
                play.sig_payload.clone(),
                vec![sp, op],
                nodes[play.to].pub_bytes.clone(),
                play.index,
            );
            nodes[play.to].sign_and_add_event(e, &play.name, index, ordered_events);
        }
    }

    fn create_hashgraph(db: bool, ordered_events: &mut [Event], peer_set: PeerSet) -> Hashgraph {
        let store: Box<dyn Store> = if db {
            Box::new(RocksDbStore::new(CACHE_SIZE, &temp_badger_dir(), false).unwrap())
        } else {
            Box::new(InmemStore::new(CACHE_SIZE))
        };

        let mut hashgraph = Hashgraph::new(
            store,
            Box::new(dummy_internal_commit_callback),
            Some(test_logger()),
        );

        hashgraph
            .init(Arc::new(peer_set))
            .expect("ERROR initializing Hashgraph");

        for (i, ev) in ordered_events.iter_mut().enumerate() {
            hashgraph
                .insert_event(ev, true)
                .unwrap_or_else(|e| panic!("ERROR inserting event {}: {}", i, e));
        }

        hashgraph
    }

    fn init_hashgraph_full(
        plays: Vec<Play>,
        db: bool,
        n: usize,
    ) -> (Hashgraph, HashMap<String, String>, Vec<Event>) {
        let (mut nodes, mut index, mut ordered_events, peer_set) = init_hashgraph_nodes(n);
        play_events(&plays, &mut nodes, &mut index, &mut ordered_events);
        let hashgraph = create_hashgraph(db, &mut ordered_events, peer_set);
        (hashgraph, index, ordered_events)
    }

    /*
    |  e12  |
    |   | \ |
    |  s10 e20
    |   | / |
    |   /   |
    | / |   |
    s00 |  s20
    |   |   |
    e01 |   |
    | \ |   |
    e0  e1  e2
    0   1   2
    */
    fn init_hashgraph() -> (Hashgraph, HashMap<String, String>) {
        let plays = vec![
            p(0, 0, "", "", "e0"),
            p(1, 0, "", "", "e1"),
            p(2, 0, "", "", "e2"),
            p(0, 1, "e0", "e1", "e01"),
            p(2, 1, "e2", "", "s20"),
            p(1, 1, "e1", "", "s10"),
            p(0, 2, "e01", "", "s00"),
            p(2, 2, "s20", "s00", "e20"),
            p(1, 2, "s10", "e20", "e12"),
        ];
        let (h, index, _) = init_hashgraph_full(plays, false, N);
        (h, index)
    }

    /// `(descendant, ancestor, expected_value, expected_error)`.
    type AncestryItem = (&'static str, &'static str, bool, bool);

    // Translation of hashgraph_test.go::TestAncestor.
    #[test]
    fn test_ancestor() {
        let (mut h, index) = init_hashgraph();
        let expected: Vec<AncestryItem> = vec![
            // first generation
            ("e01", "e0", true, false),
            ("e01", "e1", true, false),
            ("s00", "e01", true, false),
            ("s20", "e2", true, false),
            ("e20", "s00", true, false),
            ("e20", "s20", true, false),
            ("e12", "e20", true, false),
            ("e12", "s10", true, false),
            // second generation
            ("s00", "e0", true, false),
            ("s00", "e1", true, false),
            ("e20", "e01", true, false),
            ("e20", "e2", true, false),
            ("e12", "e1", true, false),
            ("e12", "s20", true, false),
            // third generation
            ("e20", "e0", true, false),
            ("e20", "e1", true, false),
            ("e20", "e2", true, false),
            ("e12", "e01", true, false),
            ("e12", "e0", true, false),
            ("e12", "e1", true, false),
            ("e12", "e2", true, false),
            // false positive
            ("e01", "e2", false, false),
            ("s00", "e2", false, false),
            ("e0", "", false, true),
            ("s00", "", false, true),
            ("e12", "", false, true),
        ];

        for (descendant, ancestor, val, exp_err) in expected {
            let (a, is_err) = match h.ancestor(&idx(&index, descendant), &idx(&index, ancestor)) {
                Ok(v) => (v, false),
                Err(_) => (false, true),
            };
            assert!(
                !(is_err && !exp_err),
                "Error computing ancestor({}, {})",
                descendant,
                ancestor
            );
            assert_eq!(a, val, "ancestor({}, {}) mismatch", descendant, ancestor);
        }
    }

    // Translation of hashgraph_test.go::TestSelfAncestor.
    #[test]
    fn test_self_ancestor() {
        let (mut h, index) = init_hashgraph();
        let expected: Vec<AncestryItem> = vec![
            ("e01", "e0", true, false),
            ("s00", "e01", true, false),
            ("e01", "e1", false, false),
            ("e12", "e20", false, false),
            ("s20", "e1", false, false),
            ("s20", "", false, true),
            ("e20", "e2", true, false),
            ("e12", "e1", true, false),
            ("e20", "e0", false, false),
            ("e12", "e2", false, false),
            ("e20", "e01", false, false),
        ];

        for (descendant, ancestor, val, exp_err) in expected {
            let (a, is_err) =
                match h.self_ancestor(&idx(&index, descendant), &idx(&index, ancestor)) {
                    Ok(v) => (v, false),
                    Err(_) => (false, true),
                };
            assert!(
                !(is_err && !exp_err),
                "Error computing self_ancestor({}, {})",
                descendant,
                ancestor
            );
            assert_eq!(
                a, val,
                "self_ancestor({}, {}) mismatch",
                descendant, ancestor
            );
        }
    }

    // Translation of hashgraph_test.go::TestSee.
    #[test]
    fn test_see() {
        let (mut h, index) = init_hashgraph();
        let expected: Vec<AncestryItem> = vec![
            ("e01", "e0", true, false),
            ("e01", "e1", true, false),
            ("e20", "e0", true, false),
            ("e20", "e01", true, false),
            ("e12", "e01", true, false),
            ("e12", "e0", true, false),
            ("e12", "e1", true, false),
            ("e12", "s20", true, false),
        ];

        for (descendant, ancestor, val, _exp_err) in expected {
            let a = h
                .see(&idx(&index, descendant), &idx(&index, ancestor))
                .unwrap_or(false);
            assert_eq!(a, val, "see({}, {}) mismatch", descendant, ancestor);
        }
    }

    // Translation of hashgraph_test.go::TestLamportTimestamp.
    #[test]
    fn test_lamport_timestamp() {
        let (mut h, index) = init_hashgraph();
        let expected: Vec<(&str, i64)> = vec![
            ("e0", 0),
            ("e1", 0),
            ("e2", 0),
            ("e01", 1),
            ("s10", 1),
            ("s20", 1),
            ("s00", 2),
            ("e20", 3),
            ("e12", 4),
        ];

        for (e, ets) in expected {
            let ts = h
                .lamport_timestamp(&idx(&index, e))
                .unwrap_or_else(|err| panic!("Error computing lamport_timestamp({}): {}", e, err));
            assert_eq!(ts, ets, "{} LamportTimestamp mismatch", e);
        }
    }

    /// Builds a `Play` carrying a transaction payload.
    fn p_tx(
        to: usize,
        index: i64,
        self_parent: &str,
        other_parent: &str,
        name: &str,
        tx_payload: Vec<Vec<u8>>,
    ) -> Play {
        Play {
            to,
            index,
            self_parent: self_parent.to_string(),
            other_parent: other_parent.to_string(),
            name: name.to_string(),
            tx_payload,
            sig_payload: Vec::new(),
        }
    }

    fn init_round_hashgraph() -> (Hashgraph, HashMap<String, String>) {
        let plays = vec![
            p(0, 0, "", "", "e0"),
            p(1, 0, "", "", "e1"),
            p(2, 0, "", "", "e2"),
            p(1, 1, "e1", "e0", "e10"),
            p(2, 1, "e2", "", "s20"),
            p(0, 1, "e0", "", "s00"),
            p(2, 2, "s20", "e10", "e21"),
            p(0, 2, "s00", "e21", "e02"),
            p(1, 2, "e10", "", "s10"),
            p(1, 3, "s10", "e02", "f1"),
            p_tx(1, 4, "f1", "", "s11", vec![b"abc".to_vec()]),
        ];
        let (h, index, _) = init_hashgraph_full(plays, false, N);

        // Set Rounds manually; normally handled by DivideRounds().
        let mut round0: std::collections::BTreeMap<String, RoundEvent> =
            std::collections::BTreeMap::new();
        for name in ["e0", "e1", "e2"] {
            round0.insert(
                idx(&index, name),
                RoundEvent {
                    witness: true,
                    famous: Trilean::Undefined,
                },
            );
        }
        h.store
            .set_round(
                0,
                &RoundInfo {
                    created_events: round0,
                    ..RoundInfo::new()
                },
            )
            .unwrap();

        let mut round1: std::collections::BTreeMap<String, RoundEvent> =
            std::collections::BTreeMap::new();
        round1.insert(
            idx(&index, "f1"),
            RoundEvent {
                witness: true,
                famous: Trilean::Undefined,
            },
        );
        h.store
            .set_round(
                1,
                &RoundInfo {
                    created_events: round1,
                    ..RoundInfo::new()
                },
            )
            .unwrap();

        (h, index)
    }

    // Translation of hashgraph_test.go::TestInsertEvent (event-coordinates and
    // undetermined-events checks).
    #[test]
    fn test_insert_event() {
        let (h, index) = init_round_hashgraph();
        let peer_set = h.store.get_peer_set(0).unwrap();
        let pks = peer_set.pub_keys();

        let coord = |name: &str, i: i64| EventCoordinates {
            hash: idx(&index, name),
            index: i,
        };

        // e0
        let e0 = h.store.get_event(&idx(&index, "e0")).unwrap();
        assert!(
            e0.body.self_parent_index == -1
                && e0.body.other_parent_creator_id == 0
                && e0.body.other_parent_index == -1
                && e0.body.creator_id == peer_set.by_pub_key[&e0.creator()].id(),
            "Invalid wire info on e0"
        );
        let e0_fd: CoordinatesMap = [
            (pks[0].clone(), coord("e0", 0)),
            (pks[1].clone(), coord("e10", 1)),
            (pks[2].clone(), coord("e21", 2)),
        ]
        .into_iter()
        .collect();
        let e0_la: CoordinatesMap = [(pks[0].clone(), coord("e0", 0))].into_iter().collect();
        assert_eq!(e0.first_descendants, e0_fd, "e0 firstDescendants mismatch");
        assert_eq!(e0.last_ancestors, e0_la, "e0 lastAncestors mismatch");

        // e21
        let e21 = h.store.get_event(&idx(&index, "e21")).unwrap();
        let e10 = h.store.get_event(&idx(&index, "e10")).unwrap();
        assert!(
            e21.body.self_parent_index == 1
                && e21.body.other_parent_creator_id == peer_set.by_pub_key[&e10.creator()].id()
                && e21.body.other_parent_index == 1
                && e21.body.creator_id == peer_set.by_pub_key[&e21.creator()].id(),
            "Invalid wire info on e21"
        );
        let e21_fd: CoordinatesMap = [
            (pks[0].clone(), coord("e02", 2)),
            (pks[1].clone(), coord("f1", 3)),
            (pks[2].clone(), coord("e21", 2)),
        ]
        .into_iter()
        .collect();
        let e21_la: CoordinatesMap = [
            (pks[0].clone(), coord("e0", 0)),
            (pks[1].clone(), coord("e10", 1)),
            (pks[2].clone(), coord("e21", 2)),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            e21.first_descendants, e21_fd,
            "e21 firstDescendants mismatch"
        );
        assert_eq!(e21.last_ancestors, e21_la, "e21 lastAncestors mismatch");

        // f1
        let f1 = h.store.get_event(&idx(&index, "f1")).unwrap();
        assert!(
            f1.body.self_parent_index == 2
                && f1.body.other_parent_creator_id == peer_set.by_pub_key[&e0.creator()].id()
                && f1.body.other_parent_index == 2
                && f1.body.creator_id == peer_set.by_pub_key[&f1.creator()].id(),
            "Invalid wire info on f1"
        );
        let f1_fd: CoordinatesMap = [(pks[1].clone(), coord("f1", 3))].into_iter().collect();
        let f1_la: CoordinatesMap = [
            (pks[0].clone(), coord("e02", 2)),
            (pks[1].clone(), coord("f1", 3)),
            (pks[2].clone(), coord("e21", 2)),
        ]
        .into_iter()
        .collect();
        assert_eq!(f1.first_descendants, f1_fd, "f1 firstDescendants mismatch");
        assert_eq!(f1.last_ancestors, f1_la, "f1 lastAncestors mismatch");

        // Check UndeterminedEvents.
        let expected_undetermined = [
            "e0", "e1", "e2", "e10", "s20", "s00", "e21", "e02", "s10", "f1", "s11",
        ];
        for (i, name) in expected_undetermined.iter().enumerate() {
            assert_eq!(
                h.undetermined_events[i],
                idx(&index, name),
                "UndeterminedEvents[{}] mismatch",
                i
            );
        }
        // 3 Events with index 0 + 1 Event with non-empty Transactions.
        assert_eq!(
            h.pending_loaded_events, 4,
            "PendingLoadedEvents should be 4"
        );
    }

    // Translation of hashgraph_test.go::TestReadWireInfo.
    #[test]
    fn test_read_wire_info() {
        let (h, index) = init_round_hashgraph();
        for (k, evh) in &index {
            let ev = h.store.get_event(evh).unwrap();
            let ev_wire = ev.to_wire();
            let ev_from_wire = h.read_wire_info(&ev_wire).unwrap();

            assert_eq!(
                ev.body.block_signatures, ev_from_wire.body.block_signatures,
                "BlockSignatures from wire mismatch for {}",
                k
            );
            assert_eq!(
                ev.body, ev_from_wire.body,
                "Body from wire mismatch for {}",
                k
            );
            assert_eq!(
                ev.signature, ev_from_wire.signature,
                "Signature from wire mismatch for {}",
                k
            );
            assert!(
                ev.verify().unwrap_or(false),
                "Error verifying signature for {}",
                k
            );
        }
    }

    // Translation of hashgraph_test.go::TestStronglySee.
    #[test]
    fn test_strongly_see() {
        let (mut h, index) = init_round_hashgraph();
        let expected: Vec<AncestryItem> = vec![
            ("e21", "e0", true, false),
            ("e02", "e10", true, false),
            ("e02", "e0", true, false),
            ("e02", "e1", true, false),
            ("f1", "e21", true, false),
            ("f1", "e10", true, false),
            ("f1", "e0", true, false),
            ("f1", "e1", true, false),
            ("f1", "e2", true, false),
            ("s11", "e2", true, false),
            ("e10", "e0", false, false),
            ("e21", "e1", false, false),
            ("e21", "e2", false, false),
            ("e02", "e2", false, false),
            ("s11", "e02", false, false),
            ("s11", "", false, true),
        ];
        let peer_set = h.store.get_peer_set(0).unwrap();

        for (descendant, ancestor, val, exp_err) in expected {
            let (a, is_err) =
                match h.strongly_see(&idx(&index, descendant), &idx(&index, ancestor), &peer_set) {
                    Ok(v) => (v, false),
                    Err(_) => (false, true),
                };
            assert!(
                !(is_err && !exp_err),
                "Error computing strongly_see({}, {})",
                descendant,
                ancestor
            );
            assert_eq!(
                a, val,
                "strongly_see({}, {}) mismatch",
                descendant, ancestor
            );
        }
    }

    // Translation of hashgraph_test.go::TestWitness.
    #[test]
    fn test_witness() {
        let (mut h, index) = init_round_hashgraph();
        let expected: Vec<(&str, bool)> = vec![
            ("e0", true),
            ("e1", true),
            ("e2", true),
            ("f1", true),
            ("e10", false),
            ("e21", false),
            ("e02", false),
        ];
        for (event, val) in expected {
            let a = h
                .witness(&idx(&index, event))
                .unwrap_or_else(|e| panic!("Error computing witness({}): {}", event, e));
            assert_eq!(a, val, "witness({}) mismatch", event);
        }
    }

    // Translation of hashgraph_test.go::TestRound.
    #[test]
    fn test_round() {
        let (mut h, index) = init_round_hashgraph();
        let expected: Vec<(&str, i64)> = vec![
            ("e0", 0),
            ("e1", 0),
            ("e2", 0),
            ("s00", 0),
            ("e10", 0),
            ("s20", 0),
            ("e21", 0),
            ("e02", 0),
            ("s10", 0),
            ("f1", 1),
            ("s11", 1),
        ];
        for (event, val) in expected {
            let r = h
                .round(&idx(&index, event))
                .unwrap_or_else(|e| panic!("Error computing round({}): {}", event, e));
            assert_eq!(r, val, "round({}) mismatch", event);
        }
    }

    // Translation of hashgraph_test.go::TestRoundDiff.
    #[test]
    fn test_round_diff() {
        let (mut h, index) = init_round_hashgraph();

        match h.round_diff(&idx(&index, "f1"), &idx(&index, "e02")) {
            Ok(d) => assert_eq!(d, 1, "RoundDiff(f1, e02) should be 1"),
            Err(e) => panic!("RoundDiff(f1, e02) returned an error: {}", e),
        }
        match h.round_diff(&idx(&index, "e02"), &idx(&index, "f1")) {
            Ok(d) => assert_eq!(d, -1, "RoundDiff(e02, f1) should be -1"),
            Err(e) => panic!("RoundDiff(e02, f1) returned an error: {}", e),
        }
        match h.round_diff(&idx(&index, "e02"), &idx(&index, "e21")) {
            Ok(d) => assert_eq!(d, 0, "RoundDiff(e02, e21) should be 0"),
            Err(e) => panic!("RoundDiff(e02, e21) returned an error: {}", e),
        }
    }

    fn re(witness: bool, famous: Trilean) -> RoundEvent {
        RoundEvent { witness, famous }
    }

    fn created(pairs: &[(String, RoundEvent)]) -> std::collections::BTreeMap<String, RoundEvent> {
        pairs.iter().cloned().collect()
    }

    // Translation of hashgraph_test.go::TestDivideRounds.
    #[test]
    fn test_divide_rounds() {
        let (mut h, index) = init_round_hashgraph();
        h.divide_rounds().expect("divide_rounds");

        assert_eq!(h.store.last_round(), 1, "last round should be 1");

        let u = Trilean::Undefined;
        let round0 = h.store.get_round(0).unwrap();
        let expected0 = created(&[
            (idx(&index, "e0"), re(true, u)),
            (idx(&index, "e1"), re(true, u)),
            (idx(&index, "e2"), re(true, u)),
            (idx(&index, "e10"), re(false, u)),
            (idx(&index, "s20"), re(false, u)),
            (idx(&index, "e21"), re(false, u)),
            (idx(&index, "s00"), re(false, u)),
            (idx(&index, "e02"), re(false, u)),
            (idx(&index, "s10"), re(false, u)),
        ]);
        assert_eq!(round0.created_events, expected0, "Round[0].CreatedEvents");

        let round1 = h.store.get_round(1).unwrap();
        let expected1 = created(&[
            (idx(&index, "f1"), re(true, u)),
            (idx(&index, "s11"), re(false, u)),
        ]);
        assert_eq!(round1.created_events, expected1, "Round[1].CreatedEvents");

        let pending = h.pending_rounds.get_ordered_pending_rounds();
        assert_eq!(
            pending,
            vec![
                PendingRound {
                    index: 0,
                    decided: false
                },
                PendingRound {
                    index: 1,
                    decided: false
                },
            ]
        );

        // [event] => (lamportTimestamp, round)
        let expected_ts: Vec<(&str, i64, i64)> = vec![
            ("e0", 0, 0),
            ("e1", 0, 0),
            ("e2", 0, 0),
            ("s00", 1, 0),
            ("e10", 1, 0),
            ("s20", 1, 0),
            ("e21", 2, 0),
            ("e02", 3, 0),
            ("s10", 2, 0),
            ("f1", 4, 1),
            ("s11", 5, 1),
        ];
        for (e, t, r) in expected_ts {
            let ev = h.store.get_event(&idx(&index, e)).unwrap();
            assert_eq!(ev.round, Some(r), "{} round", e);
            assert_eq!(ev.lamport_timestamp, Some(t), "{} lamportTimestamp", e);
        }
    }

    // Translation of hashgraph_test.go::TestCreateRoot.
    #[test]
    fn test_create_root() {
        let (mut h, index) = init_round_hashgraph();
        h.divide_rounds().unwrap();

        let root_events_map: Vec<(&str, Vec<&str>)> = vec![
            ("e0", vec!["e0"]),
            ("e02", vec!["e0", "s00", "e02"]),
            ("s10", vec!["e1", "e10", "s10"]),
            ("f1", vec!["e1", "e10", "s10", "f1"]),
        ];

        for (evh, hd) in root_events_map {
            let mut expected_root = Root::new();
            for s in &hd {
                let re_ = h.create_frame_event(&idx(&index, s)).unwrap();
                expected_root.insert(re_);
            }
            let ev = h.store.get_event(&idx(&index, evh)).unwrap();
            let root = h
                .create_root(&ev.creator(), &idx(&index, evh))
                .unwrap_or_else(|e| panic!("Error creating {} Root: {}", evh, e));
            assert_eq!(root, expected_root, "{} Root mismatch", evh);
        }
    }

    fn consensus_plays() -> Vec<Play> {
        vec![
            p(0, 0, "", "", "e0"),
            p(1, 0, "", "", "e1"),
            p(2, 0, "", "", "e2"),
            p(1, 1, "e1", "e0", "e10"),
            p_tx(2, 1, "e2", "e10", "e21", vec![b"e21".to_vec()]),
            p(2, 2, "e21", "", "e21b"),
            p(0, 1, "e0", "e21b", "e02"),
            p(1, 2, "e10", "e02", "f1"),
            p_tx(1, 3, "f1", "", "f1b", vec![b"f1b".to_vec()]),
            p(0, 2, "e02", "f1b", "f0"),
            p(2, 3, "e21b", "f1b", "f2"),
            p(1, 4, "f1b", "f0", "f10"),
            p(0, 3, "f0", "e21", "f0x"),
            p(2, 4, "f2", "f10", "f21"),
            p(0, 4, "f0x", "f21", "f02"),
            p_tx(0, 5, "f02", "", "f02b", vec![b"f02b".to_vec()]),
            p(1, 5, "f10", "f02b", "g1"),
            p(0, 6, "f02b", "g1", "g0"),
            p(2, 5, "f21", "g1", "g2"),
            p_tx(1, 6, "g1", "g0", "g10", vec![b"g10".to_vec()]),
            p(2, 6, "g2", "g10", "g21"),
            p_tx(0, 7, "g0", "g21", "g02", vec![b"g02".to_vec()]),
            p(1, 7, "g10", "g02", "h1"),
            p(0, 8, "g02", "h1", "h0"),
            p(2, 7, "g21", "h1", "h2"),
            p(1, 8, "h1", "h0", "h10"),
            p(2, 8, "h2", "h10", "h21"),
            p(0, 9, "h0", "h21", "h02"),
            p(1, 9, "h10", "h02", "i1"),
            p(0, 10, "h02", "i1", "i0"),
            p(2, 9, "h21", "i1", "i2"),
        ]
    }

    fn init_consensus_hashgraph(db: bool) -> (Hashgraph, HashMap<String, String>) {
        let (h, index, _) = init_hashgraph_full(consensus_plays(), db, N);
        (h, index)
    }

    // Translation of hashgraph_test.go::TestDivideRoundsBis.
    #[test]
    fn test_divide_rounds_bis() {
        let (mut h, index) = init_consensus_hashgraph(false);
        h.divide_rounds().expect("divide_rounds");

        let u = Trilean::Undefined;
        let expected: Vec<Vec<(&str, bool)>> = vec![
            vec![
                ("e0", true),
                ("e1", true),
                ("e2", true),
                ("e10", false),
                ("e21", false),
                ("e21b", false),
                ("e02", false),
            ],
            vec![
                ("f1", true),
                ("f1b", false),
                ("f0", true),
                ("f2", true),
                ("f10", false),
                ("f21", false),
                ("f0x", false),
                ("f02", false),
                ("f02b", false),
            ],
            vec![
                ("g1", true),
                ("g0", true),
                ("g2", true),
                ("g10", false),
                ("g21", false),
                ("g02", false),
            ],
            vec![
                ("h1", true),
                ("h0", true),
                ("h2", true),
                ("h10", false),
                ("h21", false),
                ("h02", false),
            ],
            vec![("i1", true), ("i0", true), ("i2", true)],
        ];
        for (i, names) in expected.iter().enumerate() {
            let round = h.store.get_round(i as i64).unwrap();
            let exp: std::collections::BTreeMap<String, RoundEvent> = names
                .iter()
                .map(|(name, w)| (idx(&index, name), re(*w, u)))
                .collect();
            assert_eq!(round.created_events, exp, "Round[{}].CreatedEvents", i);
        }
    }

    // Translation of hashgraph_test.go::TestDecideFame.
    #[test]
    fn test_decide_fame() {
        let (mut h, index) = init_consensus_hashgraph(false);
        h.divide_rounds().unwrap();
        h.decide_fame().expect("decide_fame");

        let u = Trilean::Undefined;
        let tt = Trilean::True;
        let expected: Vec<Vec<(&str, bool, Trilean)>> = vec![
            vec![
                ("e0", true, tt),
                ("e1", true, tt),
                ("e2", true, tt),
                ("e10", false, u),
                ("e21", false, u),
                ("e21b", false, u),
                ("e02", false, u),
            ],
            vec![
                ("f1", true, tt),
                ("f1b", false, u),
                ("f0", true, tt),
                ("f2", true, tt),
                ("f10", false, u),
                ("f21", false, u),
                ("f0x", false, u),
                ("f02", false, u),
                ("f02b", false, u),
            ],
            vec![
                ("g1", true, tt),
                ("g0", true, tt),
                ("g2", true, tt),
                ("g10", false, u),
                ("g21", false, u),
                ("g02", false, u),
            ],
            vec![
                ("h1", true, u),
                ("h0", true, u),
                ("h2", true, u),
                ("h10", false, u),
                ("h21", false, u),
                ("h02", false, u),
            ],
            vec![("i1", true, u), ("i0", true, u), ("i2", true, u)],
        ];
        for (i, names) in expected.iter().enumerate() {
            let round = h.store.get_round(i as i64).unwrap();
            let exp: std::collections::BTreeMap<String, RoundEvent> = names
                .iter()
                .map(|(name, w, f)| (idx(&index, name), re(*w, *f)))
                .collect();
            assert_eq!(round.created_events, exp, "Round[{}].CreatedEvents", i);
        }

        let expected_pending = vec![
            PendingRound {
                index: 0,
                decided: true,
            },
            PendingRound {
                index: 1,
                decided: true,
            },
            PendingRound {
                index: 2,
                decided: true,
            },
            PendingRound {
                index: 3,
                decided: false,
            },
            PendingRound {
                index: 4,
                decided: false,
            },
        ];
        assert_eq!(
            h.pending_rounds.get_ordered_pending_rounds(),
            expected_pending,
            "PendingRounds mismatch"
        );
    }

    // Translation of hashgraph_test.go::TestDecideRoundReceived.
    #[test]
    fn test_decide_round_received() {
        let (mut h, index) = init_consensus_hashgraph(false);
        h.divide_rounds().unwrap();
        h.decide_fame().unwrap();
        h.decide_round_received().expect("decide_round_received");

        let expected_received: Vec<Vec<&str>> = vec![
            vec![],
            vec!["e0", "e1", "e2", "e10", "e21", "e21b", "e02"],
            vec!["f1", "f1b", "f0", "f2", "f10", "f0x", "f21", "f02", "f02b"],
            vec![],
            vec![],
        ];
        for (i, names) in expected_received.iter().enumerate() {
            let round = h.store.get_round(i as i64).unwrap();
            let exp: Vec<String> = names.iter().map(|n| idx(&index, n)).collect();
            assert_eq!(round.received_events, exp, "Round[{}].ReceivedEvents", i);
        }

        for (name, hash) in &index {
            let e = h.store.get_event(hash).unwrap();
            if name.starts_with('e') {
                assert_eq!(e.round_received, Some(1), "{} round received", name);
            } else if name.starts_with('f') {
                assert_eq!(e.round_received, Some(2), "{} round received", name);
            } else {
                assert_eq!(
                    e.round_received, None,
                    "{} round received should be None",
                    name
                );
            }
        }

        let expected_undetermined = [
            "g1", "g0", "g2", "g10", "g21", "g02", "h1", "h0", "h2", "h10", "h21", "h02", "i1",
            "i0", "i2",
        ];
        for (i, name) in expected_undetermined.iter().enumerate() {
            assert_eq!(
                h.undetermined_events[i],
                idx(&index, name),
                "UndeterminedEvents[{}]",
                i
            );
        }
    }

    // Translation of hashgraph_test.go::TestProcessDecidedRounds.
    #[test]
    fn test_process_decided_rounds() {
        let (mut h, _index) = init_consensus_hashgraph(false);
        h.divide_rounds().unwrap();
        h.decide_fame().unwrap();
        h.decide_round_received().unwrap();
        h.process_decided_rounds().expect("process_decided_rounds");

        let consensus_events = h.store.consensus_events();
        assert_eq!(consensus_events.len(), 16, "consensus length should be 16");
        assert_eq!(
            h.pending_loaded_events, 2,
            "PendingLoadedEvents should be 2"
        );

        // Block 0
        let block0 = h.store.get_block(0).expect("Store should contain Block 0");
        assert_eq!(block0.index(), 0, "Block0 Index");
        assert_eq!(block0.round_received(), 1, "Block0 RoundReceived");
        assert_eq!(block0.transactions().len(), 1, "Block0 transaction count");
        assert_eq!(block0.transactions()[0], b"e21", "Block0.Transactions[0]");
        let frame1 = h.get_frame(block0.round_received()).unwrap();
        assert_eq!(
            block0.frame_hash(),
            frame1.hash().unwrap().as_slice(),
            "Block0 FrameHash"
        );

        // Block 1
        let block1 = h.store.get_block(1).expect("Store should contain Block 1");
        assert_eq!(block1.index(), 1, "Block1 Index");
        assert_eq!(block1.round_received(), 2, "Block1 RoundReceived");
        assert_eq!(block1.transactions().len(), 2, "Block1 transaction count");
        assert_eq!(block1.transactions()[1], b"f02b", "Block1.Transactions[1]");
        let frame2 = h.get_frame(block1.round_received()).unwrap();
        assert_eq!(
            block1.frame_hash(),
            frame2.hash().unwrap().as_slice(),
            "Block1 FrameHash"
        );

        let pending = h.pending_rounds.get_ordered_pending_rounds();
        assert_eq!(
            pending,
            vec![
                PendingRound {
                    index: 3,
                    decided: false
                },
                PendingRound {
                    index: 4,
                    decided: false
                },
            ]
        );

        assert_eq!(h.anchor_block, None, "AnchorBlock should be None");
    }

    // Translation of hashgraph_test.go::TestKnown.
    #[test]
    fn test_known() {
        let (h, _index) = init_consensus_hashgraph(false);
        let peer_set = h.store.get_peer_set(0).unwrap();
        let ids = peer_set.ids();

        let expected_known: HashMap<u32, i64> = [(ids[0], 10), (ids[1], 9), (ids[2], 9)]
            .into_iter()
            .collect();

        let known = h.store.known_events();
        for id in &ids {
            assert_eq!(known[id], expected_known[id], "Known[{}] mismatch", id);
        }
    }

    // Translation of hashgraph_test.go::TestGetFrame.
    #[test]
    fn test_get_frame() {
        let (mut h, index) = init_consensus_hashgraph(false);
        let peer_set = h.store.get_peer_set(0).unwrap();
        h.divide_rounds().unwrap();
        h.decide_fame().unwrap();
        h.decide_round_received().unwrap();
        h.process_decided_rounds().unwrap();

        // Round 1
        {
            let frame = h.get_frame(1).unwrap();
            for (_p, r) in &frame.roots {
                assert_eq!(*r, Root::new(), "Round 1 root should be empty");
            }

            let expected_hashes = ["e0", "e1", "e2", "e10", "e21", "e21b", "e02"];
            let mut expected_events: Vec<FrameEvent> = expected_hashes
                .iter()
                .map(|eh| h.create_frame_event(&idx(&index, eh)).unwrap())
                .collect();
            super::super::event::sort_frame_events(&mut expected_events);
            assert_eq!(expected_events, frame.events, "Round 1 Frame.Events");

            let timestamps: Vec<i64> = ["f0", "f1", "f2"]
                .iter()
                .map(|fw| h.store.get_event(&idx(&index, fw)).unwrap().timestamp())
                .collect();
            assert_eq!(
                common::median(&timestamps),
                frame.timestamp,
                "Round 1 Timestamp"
            );

            let block0 = h.store.get_block(0).unwrap();
            assert_eq!(
                block0.frame_hash(),
                frame.hash().unwrap().as_slice(),
                "Block0.FrameHash vs Frame1.Hash"
            );
        }

        // Round 2
        {
            let pasts: Vec<Vec<&str>> = vec![
                vec!["e0", "e02"],
                vec!["e1", "e10"],
                vec!["e2", "e21", "e21b"],
            ];
            let mut expected_roots: HashMap<String, Root> = HashMap::new();
            for (i, evs) in pasts.iter().enumerate() {
                let mut root = Root::new();
                for ev in evs {
                    root.insert(h.create_frame_event(&idx(&index, ev)).unwrap());
                }
                expected_roots.insert(peer_set.peers[i].pub_key_string(), root);
            }

            let frame = h.get_frame(2).unwrap();
            for (p, r) in &frame.roots {
                assert_eq!(*r, expected_roots[p], "Round 2 Roots[{}]", p);
            }

            let expected_hashes = ["f1", "f1b", "f0", "f2", "f10", "f0x", "f21", "f02", "f02b"];
            let mut expected_events: Vec<FrameEvent> = expected_hashes
                .iter()
                .map(|eh| h.create_frame_event(&idx(&index, eh)).unwrap())
                .collect();
            super::super::event::sort_frame_events(&mut expected_events);
            assert_eq!(expected_events, frame.events, "Round 2 Frame.Events");

            let timestamps: Vec<i64> = ["g0", "g1", "g2"]
                .iter()
                .map(|fw| h.store.get_event(&idx(&index, fw)).unwrap().timestamp())
                .collect();
            assert_eq!(
                common::median(&timestamps),
                frame.timestamp,
                "Round 2 Timestamp"
            );
        }
    }

    // Translation of hashgraph_test.go::TestResetFromFrame.
    #[test]
    fn test_reset_from_frame() {
        let (mut h, index) = init_consensus_hashgraph(false);
        let peer_set = h.store.get_peer_set(0).unwrap();
        h.divide_rounds().unwrap();
        h.decide_fame().unwrap();
        h.decide_round_received().unwrap();
        h.process_decided_rounds().unwrap();

        let block = h.store.get_block(1).unwrap();
        let frame = h.get_frame(block.round_received()).unwrap();

        // Clears the events' private fields, which must be recomputed.
        let marshalled = frame.marshal().unwrap();
        let mut unmarshalled = Frame::default();
        unmarshalled.unmarshal(&marshalled).unwrap();

        let store: Box<dyn Store> = Box::new(InmemStore::new(CACHE_SIZE));
        let mut h2 = Hashgraph::new(
            store,
            Box::new(dummy_internal_commit_callback),
            Some(test_logger()),
        );
        h2.reset(&block, &unmarshalled).expect("reset");

        // Test Known.
        let expected_known: HashMap<u32, i64> = [
            (peer_set.ids()[0], 5),
            (peer_set.ids()[1], 4),
            (peer_set.ids()[2], 4),
        ]
        .into_iter()
        .collect();
        let known = h2.store.known_events();
        for id in peer_set.by_id.keys() {
            assert_eq!(known[id], expected_known[id], "Known[{}]", id);
        }

        // Test StronglySee.
        let expected: Vec<AncestryItem> = vec![
            ("e02", "e0", true, false),
            ("e02", "e1", true, false),
            ("e21", "e0", true, false),
            ("f1", "e0", true, false),
            ("f1", "e1", true, false),
            ("f1", "e2", true, false),
        ];
        for (descendant, ancestor, val, _) in expected {
            let a = h2
                .strongly_see(&idx(&index, descendant), &idx(&index, ancestor), &peer_set)
                .unwrap_or(false);
            assert_eq!(a, val, "strongly_see({}, {})", descendant, ancestor);
        }

        // Test DivideRounds: rounds/timestamps must match the original.
        let frame_hashes: Vec<String> = frame.events.iter().map(|ev| ev.core.hex()).collect();
        for ev_hex in &frame_hashes {
            let h2r = h2.round(ev_hex).expect("h2 round");
            let hr = h.round(ev_hex).unwrap();
            assert_eq!(h2r, hr, "round mismatch after reset");

            let h2s = h2.lamport_timestamp(ev_hex).expect("h2 lamport");
            let hs = h.lamport_timestamp(ev_hex).unwrap();
            assert_eq!(h2s, hs, "lamport_timestamp mismatch after reset");
        }

        let h_round1 = h.store.get_round(1).unwrap();
        let h2_round1 = h2.store.get_round(1).unwrap();
        let mut hw = h_round1.witnesses();
        let mut h2w = h2_round1.witnesses();
        hw.sort();
        h2w.sort();
        assert_eq!(hw, h2w, "Reset Round 1 witnesses");

        // Test Consensus.
        assert_eq!(h2.store.last_block_index(), block.index(), "LastBlockIndex");
        assert_eq!(
            h2.last_consensus_round,
            Some(block.round_received()),
            "LastConsensusRound"
        );
        assert_eq!(h2.anchor_block, None, "AnchorBlock should be None");

        // Test continue after Reset.
        for r in 2..=4 {
            let round = h.store.get_round(r).unwrap();
            let mut events: Vec<Event> = round
                .created_events
                .keys()
                .map(|e| h.store.get_event(e).unwrap())
                .collect();
            super::super::event::sort_by_topological_order(&mut events);
            for ev in &events {
                let marshalled_ev = ev.marshal_db().unwrap();
                let mut unmarshalled_ev = Event::default();
                unmarshalled_ev.unmarshal_db(&marshalled_ev).unwrap();
                h2.insert_event_and_run_consensus(&mut unmarshalled_ev, true)
                    .expect("inserting event after reset");
            }
        }

        for r in 1..=4 {
            let h_round = h.store.get_round(r).unwrap();
            let h2_round = h2.store.get_round(r).unwrap();
            let mut hw = h_round.witnesses();
            let mut h2w = h2_round.witnesses();
            hw.sort();
            h2w.sort();
            assert_eq!(hw, h2w, "Reset Round {} witnesses", r);
        }
    }

    // Translation of hashgraph_test.go::TestBootstrap.
    #[test]
    fn test_bootstrap() {
        let dir = temp_badger_dir();

        // Build a first Hashgraph with a RocksDB backend; run consensus.
        let (mut nodes, mut index, mut ordered_events, peer_set) = init_hashgraph_nodes(N);
        play_events(
            &consensus_plays(),
            &mut nodes,
            &mut index,
            &mut ordered_events,
        );
        let store: Box<dyn Store> = Box::new(RocksDbStore::new(CACHE_SIZE, &dir, false).unwrap());
        let mut h = Hashgraph::new(
            store,
            Box::new(dummy_internal_commit_callback),
            Some(test_logger()),
        );
        h.init(Arc::new(peer_set)).unwrap();
        for ev in ordered_events.iter_mut() {
            h.insert_event(ev, true).unwrap();
        }
        h.divide_rounds().unwrap();
        h.decide_fame().unwrap();
        h.decide_round_received().unwrap();
        h.process_decided_rounds().unwrap();

        // Capture h's state, then close + drop it to release the RocksDB lock.
        let h_consensus_events = h.store.consensus_events();
        let h_known = h.store.known_events();
        let h_last_consensus_round = h.last_consensus_round;
        let h_last_commited = h.last_commited_round_events;
        let h_consensus_txs = h.consensus_transactions;
        let h_pending_loaded = h.pending_loaded_events;
        h.store.close().unwrap();
        drop(h);

        // Reopen the database and bootstrap a fresh Hashgraph from it.
        let recycled: Box<dyn Store> =
            Box::new(RocksDbStore::new(CACHE_SIZE, &dir, false).unwrap());
        let mut nh = Hashgraph::new(recycled, Box::new(dummy_internal_commit_callback), None);
        nh.bootstrap().expect("bootstrap");

        assert_eq!(
            nh.store.consensus_events().len(),
            h_consensus_events.len(),
            "bootstrapped consensus event count"
        );
        assert_eq!(nh.store.known_events(), h_known, "bootstrapped Known");
        assert_eq!(
            nh.last_consensus_round, h_last_consensus_round,
            "bootstrapped LastConsensusRound"
        );
        assert_eq!(
            nh.last_commited_round_events, h_last_commited,
            "bootstrapped LastCommitedRoundEvents"
        );
        assert_eq!(
            nh.consensus_transactions, h_consensus_txs,
            "bootstrapped ConsensusTransactions"
        );
        assert_eq!(
            nh.pending_loaded_events, h_pending_loaded,
            "bootstrapped PendingLoadedEvents"
        );

        nh.store.close().unwrap();
        drop(nh);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
