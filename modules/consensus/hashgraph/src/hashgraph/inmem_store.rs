//! Translation of `chain/hashgraph/inmem_store.go`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::block::Block;
use super::caches::{ParticipantEventsCache, PeerSetCache};
use super::event::Event;
use super::frame::Frame;
use super::root::Root;
use super::round_info::RoundInfo;
use super::store::Store;
use crate::common::{LRU, RollingIndex};
use crate::common::{StoreErrType, is_store, new_store_err};
use crate::peers::{Peer, PeerSet};

/// The mutable state behind [`InmemStore`].
struct InmemStoreInner {
    cache_size: i64,
    event_cache: LRU<String, Event>,
    round_cache: LRU<i64, RoundInfo>,
    block_cache: LRU<i64, Block>,
    frame_cache: LRU<i64, Frame>,
    consensus_cache: RollingIndex<String>,
    tot_consensus_events: i64,
    peer_set_cache: PeerSetCache,
    participant_events_cache: ParticipantEventsCache,
    roots: HashMap<String, Arc<Mutex<Root>>>,
    last_round: i64,
    last_consensus_events: HashMap<String, String>,
    last_block: i64,
}

/// Frames are far heavier than the other cached items — each one is a full
/// consensus snapshot (a `BTreeMap` of roots plus a `Vec<Event>`, every event
/// carrying its own maps). Caching `cache_size` (10 000 by default) of them
/// pinned hundreds of MB to GBs of ever-growing snapshots in RAM — the bulk of
/// the node's daylong memory climb (heaptrack traced it to `set_frame`'s frame
/// clone). Frames are persisted to RocksDB and re-read on a miss (see
/// `RocksDbStore::get_frame`), so only a small hot set needs to stay resident:
/// the last 25 consensus rounds' frames by default, env-tunable through
/// `CASPAR_BABBLE_FRAME_CACHE`.
const DEFAULT_MAX_FRAME_CACHE: usize = 25;

fn max_frame_cache() -> usize {
    aseman_config::legacy_adapter_snapshot()
        .map(|config| config.babble_frame_cache)
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_FRAME_CACHE)
}

impl InmemStoreInner {
    fn new(cache_size: i64) -> InmemStoreInner {
        let cs = cache_size.max(0) as usize;
        InmemStoreInner {
            cache_size,
            event_cache: LRU::new(cs, None),
            round_cache: LRU::new(cs, None),
            block_cache: LRU::new(cs, None),
            frame_cache: LRU::new(cs.min(max_frame_cache()), None),
            consensus_cache: RollingIndex::new("ConsensusCache", cs),
            tot_consensus_events: 0,
            peer_set_cache: PeerSetCache::new(),
            participant_events_cache: ParticipantEventsCache::new(cs),
            roots: HashMap::new(),
            last_round: -1,
            last_block: -1,
            last_consensus_events: HashMap::new(),
        }
    }

    fn get_peer_set(&self, round: i64) -> Result<Arc<PeerSet>> {
        self.peer_set_cache.get(round)
    }

    fn set_peer_set(&mut self, round: i64, peer_set: Arc<PeerSet>) -> Result<()> {
        self.peer_set_cache.set(round, peer_set.clone())?;
        for p in &peer_set.peers {
            self.add_participant(p)?;
        }
        Ok(())
    }

    fn add_participant(&mut self, p: &Peer) -> Result<()> {
        if !self
            .participant_events_cache
            .participants
            .by_id
            .contains_key(&p.id())
        {
            self.participant_events_cache.add_peer(p)?;
        }
        self.roots
            .entry(p.pub_key_string())
            .or_insert_with(|| Arc::new(Mutex::new(Root::new())));
        Ok(())
    }

    fn get_event(&mut self, key: &str) -> Result<Event> {
        match self.event_cache.get(&key.to_string()) {
            Some(ev) => Ok(ev),
            None => Err(new_store_err("EventCache", StoreErrType::KeyNotFound, key).into()),
        }
    }

    fn set_event(&mut self, event: &Event) -> Result<()> {
        let key = event.hex();
        let existing = self.get_event(&key);
        match &existing {
            Err(e) if !is_store(e, StoreErrType::KeyNotFound) => {
                return Err(anyhow::anyhow!("{}", e));
            }
            Err(_) => {
                // KeyNotFound — a brand new event.
                self.participant_events_cache
                    .set(&event.creator(), &key, event.index())?;
            }
            Ok(_) => {}
        }
        self.event_cache.add(key, event.clone());
        Ok(())
    }

    fn add_consensus_event(&mut self, event: &Event) -> Result<()> {
        let _ = self
            .consensus_cache
            .set(event.hex(), self.tot_consensus_events);
        self.tot_consensus_events += 1;
        self.last_consensus_events
            .insert(event.creator(), event.hex());
        Ok(())
    }

    fn get_round(&mut self, r: i64) -> Result<RoundInfo> {
        match self.round_cache.get(&r) {
            Some(ri) => Ok(ri),
            None => {
                Err(new_store_err("RoundCache", StoreErrType::KeyNotFound, &r.to_string()).into())
            }
        }
    }

    fn set_round(&mut self, r: i64, round: &RoundInfo) -> Result<()> {
        self.round_cache.add(r, round.clone());
        if r > self.last_round {
            self.last_round = r;
        }
        Ok(())
    }

    fn get_block(&mut self, index: i64) -> Result<Block> {
        match self.block_cache.get(&index) {
            Some(b) => Ok(b),
            None => Err(
                new_store_err("BlockCache", StoreErrType::KeyNotFound, &index.to_string()).into(),
            ),
        }
    }

    fn set_block(&mut self, block: &Block) -> Result<()> {
        let index = block.index();
        if let Err(e) = self.get_block(index) {
            if !is_store(&e, StoreErrType::KeyNotFound) {
                return Err(e);
            }
        }
        self.block_cache.add(index, block.clone());
        if index > self.last_block {
            self.last_block = index;
        }
        Ok(())
    }

    fn get_frame(&mut self, index: i64) -> Result<Frame> {
        match self.frame_cache.get(&index) {
            Some(fr) => Ok(fr),
            None => Err(
                new_store_err("FrameCache", StoreErrType::KeyNotFound, &index.to_string()).into(),
            ),
        }
    }

    fn set_frame(&mut self, frame: &Frame) -> Result<()> {
        let index = frame.round;
        if let Err(e) = self.get_frame(index) {
            if !is_store(&e, StoreErrType::KeyNotFound) {
                return Err(e);
            }
        }
        self.frame_cache.add(index, frame.clone());
        Ok(())
    }

    fn reset(&mut self, frame: &Frame) -> Result<()> {
        let cs = self.cache_size.max(0) as usize;
        self.peer_set_cache = PeerSetCache::new();
        self.event_cache = LRU::new(cs, None);
        self.round_cache = LRU::new(cs, None);
        self.block_cache = LRU::new(cs, None);
        self.frame_cache = LRU::new(cs.min(max_frame_cache()), None);
        self.participant_events_cache = ParticipantEventsCache::new(cs);
        self.roots = HashMap::new();
        self.last_round = -1;
        self.last_block = -1;
        self.consensus_cache = RollingIndex::new("ConsensusCache", cs);
        self.last_consensus_events = HashMap::new();

        // Set Roots from Frame.
        self.roots = frame
            .roots
            .iter()
            .map(|(k, v)| (k.clone(), Arc::new(Mutex::new(v.clone()))))
            .collect();

        for (round, ps) in &frame.peer_sets {
            self.set_peer_set(*round, Arc::new(PeerSet::new(ps.clone())))?;
        }

        self.set_frame(frame)
    }
}

/// Implements [`Store`] with in-memory caches. When the caches are full, older
/// items are evicted, so `InmemStore` is not suitable for long running
/// deployments where joining nodes expect to sync from the beginning of a
/// hashgraph.
pub struct InmemStore {
    inner: RefCell<InmemStoreInner>,
}

impl InmemStore {
    /// Creates a new `InmemStore` where all caches are limited by `cache_size`.
    pub fn new(cache_size: i64) -> InmemStore {
        InmemStore {
            inner: RefCell::new(InmemStoreInner::new(cache_size)),
        }
    }
}

impl Store for InmemStore {
    fn cache_size(&self) -> i64 {
        self.inner.borrow().cache_size
    }

    fn get_peer_set(&self, round: i64) -> Result<Arc<PeerSet>> {
        self.inner.borrow().get_peer_set(round)
    }

    fn set_peer_set(&self, round: i64, peers: Arc<PeerSet>) -> Result<()> {
        self.inner.borrow_mut().set_peer_set(round, peers)
    }

    fn get_all_peer_sets(&self) -> Result<HashMap<i64, Vec<Peer>>> {
        self.inner.borrow().peer_set_cache.get_all()
    }

    fn first_round(&self, id: u32) -> (i64, bool) {
        self.inner.borrow().peer_set_cache.first_round(id)
    }

    fn repertoire_by_pub_key(&self) -> HashMap<String, Peer> {
        self.inner.borrow().peer_set_cache.repertoire_by_pub_key()
    }

    fn repertoire_by_id(&self) -> HashMap<u32, Peer> {
        self.inner.borrow().peer_set_cache.repertoire_by_id()
    }

    fn get_event(&self, hash: &str) -> Result<Event> {
        self.inner.borrow_mut().get_event(hash)
    }

    fn set_event(&self, event: &Event) -> Result<()> {
        self.inner.borrow_mut().set_event(event)
    }

    fn participant_events(&self, participant: &str, skip: i64) -> Result<Vec<String>> {
        self.inner
            .borrow()
            .participant_events_cache
            .get(participant, skip)
    }

    fn participant_event(&self, participant: &str, index: i64) -> Result<String> {
        self.inner
            .borrow()
            .participant_events_cache
            .get_item(participant, index)
    }

    fn last_event_from(&self, participant: &str) -> Result<String> {
        self.inner
            .borrow()
            .participant_events_cache
            .get_last(participant)
    }

    fn last_consensus_event_from(&self, participant: &str) -> Result<String> {
        Ok(self
            .inner
            .borrow()
            .last_consensus_events
            .get(participant)
            .cloned()
            .unwrap_or_default())
    }

    fn known_events(&self) -> HashMap<u32, i64> {
        self.inner.borrow().participant_events_cache.known()
    }

    fn consensus_events(&self) -> Vec<String> {
        let (last_window, _) = self.inner.borrow().consensus_cache.get_last_window();
        last_window
    }

    fn consensus_events_count(&self) -> i64 {
        self.inner.borrow().tot_consensus_events
    }

    fn add_consensus_event(&self, event: &Event) -> Result<()> {
        self.inner.borrow_mut().add_consensus_event(event)
    }

    fn get_round(&self, round_index: i64) -> Result<RoundInfo> {
        self.inner.borrow_mut().get_round(round_index)
    }

    fn set_round(&self, round_index: i64, round_info: &RoundInfo) -> Result<()> {
        self.inner.borrow_mut().set_round(round_index, round_info)
    }

    fn last_round(&self) -> i64 {
        self.inner.borrow().last_round
    }

    fn round_witnesses(&self, round_index: i64) -> Vec<String> {
        match self.inner.borrow_mut().get_round(round_index) {
            Ok(round) => round.witnesses(),
            Err(_) => Vec::new(),
        }
    }

    fn round_events(&self, round_index: i64) -> i64 {
        match self.inner.borrow_mut().get_round(round_index) {
            Ok(round) => round.created_events.len() as i64,
            Err(_) => 0,
        }
    }

    fn get_root(&self, participant: &str) -> Result<Arc<Mutex<Root>>> {
        match self.inner.borrow().roots.get(participant) {
            Some(r) => Ok(r.clone()),
            None => Err(new_store_err("RootCache", StoreErrType::KeyNotFound, participant).into()),
        }
    }

    fn get_block(&self, index: i64) -> Result<Block> {
        self.inner.borrow_mut().get_block(index)
    }

    fn set_block(&self, block: &Block) -> Result<()> {
        self.inner.borrow_mut().set_block(block)
    }

    fn last_block_index(&self) -> i64 {
        self.inner.borrow().last_block
    }

    fn get_frame(&self, round_received: i64) -> Result<Frame> {
        self.inner.borrow_mut().get_frame(round_received)
    }

    fn set_frame(&self, frame: &Frame) -> Result<()> {
        self.inner.borrow_mut().set_frame(frame)
    }

    fn reset(&self, frame: &Frame) -> Result<()> {
        self.inner.borrow_mut().reset(frame)
    }

    fn close(&self) -> Result<()> {
        Ok(())
    }

    fn store_path(&self) -> String {
        String::new()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use crate::crypto::keys;
    use crate::peers::{Peer, PeerSet};
    use k256::ecdsa::SigningKey;

    /// Test-only mirror of the Go `participant` struct.
    pub struct Participant {
        pub id: u32,
        pub priv_key: SigningKey,
        pub pub_key: Vec<u8>,
        pub hex: String,
    }

    /// Translation of the shared `initPeers` test helper.
    pub fn init_peers(n: usize) -> (PeerSet, Vec<Participant>) {
        let mut pirs: Vec<Peer> = Vec::new();
        let mut participants: Vec<Participant> = Vec::new();

        for _ in 0..n {
            let key = keys::generate_ecdsa_key().unwrap();
            let pub_key = keys::from_public_key(key.verifying_key());
            let peer = Peer::new(&keys::public_key_hex(key.verifying_key()), "", "");
            participants.push(Participant {
                id: peer.id(),
                priv_key: key,
                pub_key,
                hex: peer.pub_key_string(),
            });
            pirs.push(peer);
        }

        (PeerSet::new(pirs), participants)
    }

    /// Translation of the shared `contains` test helper.
    pub fn contains(slice: &[String], item: &str) -> bool {
        slice.iter().any(|s| s == item)
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::{contains, init_peers};
    use super::*;
    use crate::hashgraph::block::{Block, BlockSignature};
    use crate::hashgraph::event::Event;
    use crate::hashgraph::internal_transaction::{InternalTransaction, TransactionType};
    use crate::hashgraph::round_info::RoundInfo;
    use crate::peers::Peer;

    // Translation of inmem_store_test.go::TestInmemEvents.
    #[test]
    fn test_inmem_events() {
        let n = 3usize;
        let cache_size = 100i64;
        let test_size = 15i64;

        let store = InmemStore::new(cache_size);
        let (peer_set, participants) = init_peers(n);
        store.set_peer_set(0, Arc::new(peer_set)).unwrap();

        let mut events: HashMap<String, Vec<Event>> = HashMap::new();

        // Store Events.
        for p in &participants {
            let mut items: Vec<Event> = Vec::new();
            for k in 0..test_size {
                let event = Event::new(
                    vec![format!("{}_{}", &p.hex[..5], k).into_bytes()],
                    vec![],
                    vec![BlockSignature {
                        validator: b"validator".to_vec(),
                        index: 0,
                        signature: "r|s".to_string(),
                    }],
                    vec![String::new(), String::new()],
                    p.pub_key.clone(),
                    k,
                );
                let _ = event.hex(); // set private caches
                store.set_event(&event).unwrap();
                items.push(event);
            }
            events.insert(p.hex.clone(), items);
        }

        for (_p, evs) in &events {
            for ev in evs {
                let rev = store.get_event(&ev.hex()).unwrap();
                assert_eq!(ev.body, rev.body, "stored event body mismatch");
            }
        }

        // Check ParticipantEventsCache.
        let skip_index = -1i64;
        for p in &participants {
            let p_events = store.participant_events(&p.hex, skip_index).unwrap();
            assert_eq!(p_events.len() as i64, test_size, "participant event count");

            let expected_events = &events[&p.hex][(skip_index + 1) as usize..];
            for (k, e) in expected_events.iter().enumerate() {
                assert_eq!(e.hex(), p_events[k], "ParticipantEvents mismatch");
            }
        }

        // Check KnownEvents.
        let known = store.known_events();
        for p in &participants {
            assert_eq!(known[&p.id], test_size - 1, "Incorrect Known");
        }

        // Add ConsensusEvents.
        for p in &participants {
            for ev in &events[&p.hex] {
                store.add_consensus_event(ev).unwrap();
            }
        }
    }

    // Translation of inmem_store_test.go::TestInmemRounds.
    #[test]
    fn test_inmem_rounds() {
        let n = 3usize;
        let store = InmemStore::new(100);
        let (peer_set, participants) = init_peers(n);
        store.set_peer_set(0, Arc::new(peer_set)).unwrap();

        let mut round = RoundInfo::new();
        for p in &participants {
            let event = Event::new(
                vec![],
                vec![],
                vec![],
                vec![String::new(), String::new()],
                p.pub_key.clone(),
                0,
            );
            round.add_created_event(&event.hex(), true);
        }

        // Store Round.
        store.set_round(0, &round).unwrap();
        let stored_round = store.get_round(0).unwrap();
        assert_eq!(round, stored_round, "Round and StoredRound do not match");

        // Check LastRound.
        assert_eq!(store.last_round(), 0, "Store LastRound should be 0");

        // Check witnesses.
        let witnesses = store.round_witnesses(0);
        let expected_witnesses = round.witnesses();
        assert_eq!(
            witnesses.len(),
            expected_witnesses.len(),
            "witness count mismatch"
        );
        for w in &expected_witnesses {
            assert!(contains(&witnesses, w), "Witnesses should contain {}", w);
        }
    }

    // Translation of inmem_store_test.go::TestInmemBlocks.
    #[test]
    fn test_inmem_blocks() {
        let n = 3usize;
        let store = InmemStore::new(100);
        let (peer_set, participants) = init_peers(n);
        store.set_peer_set(0, Arc::new(peer_set)).unwrap();

        let index = 0i64;
        let round_received = 7i64;
        let transactions: Vec<Vec<u8>> = vec![
            b"tx1".to_vec(),
            b"tx2".to_vec(),
            b"tx3".to_vec(),
            b"tx4".to_vec(),
            b"tx5".to_vec(),
        ];
        let internal_transactions = vec![
            InternalTransaction::new(
                TransactionType::PeerAdd,
                Peer::new("0XBAAAAAAAD", "paris", ""),
            ),
            InternalTransaction::new(
                TransactionType::PeerRemove,
                Peer::new("0XB16B00B5", "london", ""),
            ),
        ];
        let frame_hash = b"this is the frame hash".to_vec();

        let mut block = Block::new(
            index,
            round_received,
            frame_hash,
            vec![],
            transactions,
            internal_transactions,
            0,
        );

        let sig1 = block.sign(&participants[0].priv_key).unwrap();
        let sig2 = block.sign(&participants[1].priv_key).unwrap();
        block.set_signature(sig1.clone()).unwrap();
        block.set_signature(sig2.clone()).unwrap();

        // Store Block.
        store.set_block(&block).unwrap();
        let stored_block = store.get_block(index).unwrap();
        assert_eq!(stored_block.body, block.body, "Block bodies do not match");
        assert_eq!(
            stored_block.signatures, block.signatures,
            "Block signatures do not match"
        );

        // Check signatures in stored Block.
        let val1_sig = stored_block
            .signatures
            .get(&participants[0].hex)
            .expect("Validator1 signature not stored");
        assert_eq!(
            *val1_sig, sig1.signature,
            "Validator1 block signatures differ"
        );

        let val2_sig = stored_block
            .signatures
            .get(&participants[1].hex)
            .expect("Validator2 signature not stored");
        assert_eq!(
            *val2_sig, sig2.signature,
            "Validator2 block signatures differ"
        );
    }
}
