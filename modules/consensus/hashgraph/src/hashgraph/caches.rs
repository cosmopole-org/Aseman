//! Translation of `chain/hashgraph/caches.go`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;

use super::block::BlockSignature;
use crate::common::{RollingIndexMap, StoreErrType, new_store_err};
use crate::peers::{Peer, PeerSet};

/// A two-component memoization key used by the hashgraph caches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub x: String,
    pub y: String,
}

impl Key {
    pub fn new(x: &str, y: &str) -> Key {
        Key {
            x: x.to_string(),
            y: y.to_string(),
        }
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{{{}, {}}}", self.x, self.y)
    }
}

/// A three-component memoization key used by the hashgraph caches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TreKey {
    pub x: String,
    pub y: String,
    pub z: String,
}

impl TreKey {
    pub fn new(x: &str, y: &str, z: &str) -> TreKey {
        TreKey {
            x: x.to_string(),
            y: y.to_string(),
            z: z.to_string(),
        }
    }
}

impl std::fmt::Display for TreKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{{{}, {}, {}}}", self.x, self.y, self.z)
    }
}

// ---------------------------------------------------------------------------
// ParticipantEventsCache
// ---------------------------------------------------------------------------

/// A cache associated with a peer-set that keeps a `RollingIndex` of events for
/// every peer.
pub struct ParticipantEventsCache {
    pub participants: PeerSet,
    rim: RollingIndexMap<String>,
}

impl ParticipantEventsCache {
    /// Instantiates a new `ParticipantEventsCache`. `size` controls the size of
    /// each `RollingIndex` within the cache.
    pub fn new(size: usize) -> ParticipantEventsCache {
        ParticipantEventsCache {
            participants: PeerSet::new(vec![]),
            rim: RollingIndexMap::new("ParticipantEvents", size),
        }
    }

    /// Adds a peer to the cache.
    pub fn add_peer(&mut self, peer: &Peer) -> Result<()> {
        self.participants = self.participants.with_new_peer(peer);
        self.rim.add_key(peer.id())
    }

    /// Resolves the (case-insensitive) hex public key to a participant id.
    fn participant_id(&self, participant: &str) -> Result<u32> {
        let p_upper = participant.to_uppercase();
        match self.participants.by_pub_key.get(&p_upper) {
            Some(peer) => Ok(peer.id()),
            None => Err(new_store_err(
                "ParticipantEvents",
                StoreErrType::UnknownParticipant,
                &p_upper,
            )
            .into()),
        }
    }

    /// Returns a participant's events with index > `skip_index`.
    pub fn get(&self, participant: &str, skip_index: i64) -> Result<Vec<String>> {
        let id = self.participant_id(participant)?;
        self.rim.get(id, skip_index)
    }

    /// Returns a specific event for a specific peer.
    pub fn get_item(&self, participant: &str, index: i64) -> Result<String> {
        let id = self.participant_id(participant)?;
        self.rim.get_item(id, index)
    }

    /// Returns the index of a participant's last event.
    pub fn get_last(&self, participant: &str) -> Result<String> {
        let id = self.participant_id(participant)?;
        self.rim.get_last(id)
    }

    /// Attempts to set an event into a participant's events.
    pub fn set(&mut self, participant: &str, hash: &str, index: i64) -> Result<()> {
        let id = self.participant_id(participant)?;
        self.rim.set(id, hash.to_string(), index)
    }

    /// Returns `[participant id] => last known index`.
    pub fn known(&self) -> HashMap<u32, i64> {
        self.rim.known()
    }
}

// ---------------------------------------------------------------------------
// PeerSetCache
// ---------------------------------------------------------------------------

/// A cache that keeps track of peer-sets.
pub struct PeerSetCache {
    rounds: Vec<i64>,
    peer_sets: HashMap<i64, Arc<PeerSet>>,
    repertoire_by_pub_key: HashMap<String, Peer>,
    repertoire_by_id: HashMap<u32, Peer>,
    first_rounds: HashMap<u32, i64>,
}

impl Default for PeerSetCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerSetCache {
    /// Creates a new `PeerSetCache`.
    pub fn new() -> PeerSetCache {
        PeerSetCache {
            rounds: Vec::new(),
            peer_sets: HashMap::new(),
            repertoire_by_pub_key: HashMap::new(),
            repertoire_by_id: HashMap::new(),
            first_rounds: HashMap::new(),
        }
    }

    /// Adds a peer-set at a given round and updates internal information.
    pub fn set(&mut self, round: i64, peer_set: Arc<PeerSet>) -> Result<()> {
        if self.peer_sets.contains_key(&round) {
            return Err(new_store_err(
                "PeerSetCache",
                StoreErrType::KeyAlreadyExists,
                &round.to_string(),
            )
            .into());
        }

        self.peer_sets.insert(round, peer_set.clone());

        self.rounds.push(round);
        self.rounds.sort_unstable();

        for p in &peer_set.peers {
            self.repertoire_by_pub_key
                .insert(p.pub_key_string(), p.clone());
            self.repertoire_by_id.insert(p.id(), p.clone());
            let fr = self.first_rounds.get(&p.id()).copied();
            if fr.is_none() || fr.unwrap() > round {
                self.first_rounds.insert(p.id(), round);
            }
        }

        Ok(())
    }

    /// Returns the peer-set corresponding to a given round.
    pub fn get(&self, round: i64) -> Result<Arc<PeerSet>> {
        // check if directly in peer_sets
        if let Some(ps) = self.peer_sets.get(&round) {
            return Ok(ps.clone());
        }

        // situate round in sorted rounds
        if self.rounds.is_empty() {
            return Err(new_store_err(
                "PeerSetCache",
                StoreErrType::KeyNotFound,
                &round.to_string(),
            )
            .into());
        }

        if round < self.rounds[0] {
            return Ok(self.peer_sets[&self.rounds[0]].clone());
        }

        for i in 0..self.rounds.len() - 1 {
            if round >= self.rounds[i] && round < self.rounds[i + 1] {
                return Ok(self.peer_sets[&self.rounds[i]].clone());
            }
        }

        // return last PeerSet
        Ok(self.peer_sets[&self.rounds[self.rounds.len() - 1]].clone())
    }

    /// Returns all peer-sets, keyed by round.
    pub fn get_all(&self) -> Result<HashMap<i64, Vec<Peer>>> {
        let mut res = HashMap::new();
        for r in &self.rounds {
            res.insert(*r, self.peer_sets[r].peers.clone());
        }
        Ok(res)
    }

    /// Returns all known peers indexed by ID, including peers no longer in the
    /// active peer-set.
    pub fn repertoire_by_id(&self) -> HashMap<u32, Peer> {
        self.repertoire_by_id.clone()
    }

    /// Returns all known peers indexed by public key.
    pub fn repertoire_by_pub_key(&self) -> HashMap<String, Peer> {
        self.repertoire_by_pub_key.clone()
    }

    /// Returns the index of the first round where a peer appeared.
    pub fn first_round(&self, id: u32) -> (i64, bool) {
        match self.first_rounds.get(&id) {
            Some(fr) => (*fr, true),
            None => (i32::MAX as i64, false),
        }
    }
}

// ---------------------------------------------------------------------------
// PendingRound / PendingRoundsCache
// ---------------------------------------------------------------------------

/// Represents a round as it goes through consensus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PendingRound {
    pub index: i64,
    pub decided: bool,
}

/// A cache for `PendingRound`s.
pub struct PendingRoundsCache {
    items: HashMap<i64, PendingRound>,
}

impl Default for PendingRoundsCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingRoundsCache {
    /// Creates a new `PendingRoundsCache`.
    pub fn new() -> PendingRoundsCache {
        PendingRoundsCache {
            items: HashMap::new(),
        }
    }

    /// Indicates whether a round is already part of the cache.
    pub fn queued(&self, round: i64) -> bool {
        self.items.contains_key(&round)
    }

    /// Adds an item to the cache.
    pub fn set(&mut self, pending_round: PendingRound) {
        self.items.insert(pending_round.index, pending_round);
    }

    /// Returns the ordered list of `PendingRound`s.
    pub fn get_ordered_pending_rounds(&self) -> Vec<PendingRound> {
        let mut sorted: Vec<PendingRound> = self.items.values().copied().collect();
        sorted.sort_by(|a, b| a.index.cmp(&b.index));
        sorted
    }

    /// Marks decided rounds.
    pub fn update(&mut self, decided_rounds: &[i64]) {
        for drn in decided_rounds {
            if let Some(dr) = self.items.get_mut(drn) {
                dr.decided = true;
            }
        }
    }

    /// Removes a list of rounds from the cache.
    pub fn clean(&mut self, processed_rounds: &[i64]) {
        for pr in processed_rounds {
            self.items.remove(pr);
        }
    }
}

// ---------------------------------------------------------------------------
// SigPool
// ---------------------------------------------------------------------------

/// Holds a collection of `BlockSignature`s.
#[derive(Default)]
pub struct SigPool {
    items: HashMap<String, BlockSignature>,
}

impl SigPool {
    /// Creates a new `SigPool`.
    pub fn new() -> SigPool {
        SigPool {
            items: HashMap::new(),
        }
    }

    /// Adds an item to the pool.
    pub fn add(&mut self, block_signature: BlockSignature) {
        self.items.insert(block_signature.key(), block_signature);
    }

    /// Removes an item from the pool.
    pub fn remove(&mut self, key: &str) {
        self.items.remove(key);
    }

    /// Removes multiple items from the pool.
    pub fn remove_slice(&mut self, sigs: &[BlockSignature]) {
        for s in sigs {
            self.items.remove(&s.key());
        }
    }

    /// Returns the number of items in the pool.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Returns the contents of the pool as a map.
    pub fn items(&self) -> HashMap<String, BlockSignature> {
        self.items.clone()
    }

    /// Returns the contents of the pool as a slice, in no particular order.
    pub fn slice(&self) -> Vec<BlockSignature> {
        self.items.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{StoreErrType, is_store};
    use crate::peers::Peer;
    use std::collections::HashMap;

    // Translation of caches_test.go::TestParticipantEventsCache.
    #[test]
    fn test_participant_events_cache() {
        let size = 20usize;
        let test_size = 25i64;
        let participants = PeerSet::new(vec![
            Peer::new("0xaa", "", ""),
            Peer::new("0xbb", "", ""),
            Peer::new("0xcc", "", ""),
        ]);

        let mut pec = ParticipantEventsCache::new(size);
        for p in &participants.peers {
            pec.add_peer(p).unwrap();
        }

        let pub_keys: Vec<String> = participants.by_pub_key.keys().cloned().collect();
        let mut items: HashMap<String, Vec<String>> = HashMap::new();
        for pk in &pub_keys {
            items.insert(pk.clone(), Vec::new());
        }

        for i in 0..test_size {
            for pk in &pub_keys {
                let item = format!("{}{}", pk, i);
                pec.set(pk, &item, i).unwrap();
                items.get_mut(pk).unwrap().push(item);
            }
        }

        for pk in &pub_keys {
            let err = pec.get_item(pk, 9).unwrap_err();
            assert!(is_store(&err, StoreErrType::TooLate), "Expected TooLate");

            let index2 = 15usize;
            let actual2 = pec.get_item(pk, index2 as i64).unwrap();
            assert_eq!(actual2, items[pk][index2], "get_item mismatch");

            let actual3 = pec.get(pk, 27).unwrap();
            assert!(actual3.is_empty(), "expected empty");
        }

        let known = pec.known();
        for (_p, k) in known {
            assert_eq!(k, test_size - 1, "Known last index mismatch");
        }

        for pk in &pub_keys {
            match pec.get(pk, 0) {
                Ok(_) => {}
                Err(e) => assert!(is_store(&e, StoreErrType::TooLate), "Skipping 0 -> TooLate"),
            }

            let skip_index = 9usize;
            let expected = &items[pk][skip_index + 1..];
            let cached = pec.get(pk, skip_index as i64).unwrap();
            assert_eq!(cached.as_slice(), expected, "get mismatch");

            let skip_index2 = 15usize;
            let expected2 = &items[pk][skip_index2 + 1..];
            let cached2 = pec.get(pk, skip_index2 as i64).unwrap();
            assert_eq!(cached2.as_slice(), expected2, "get mismatch");

            let cached3 = pec.get(pk, 27).unwrap();
            assert!(cached3.is_empty(), "expected empty");
        }
    }

    // Translation of caches_test.go::TestParticipantEventsCacheEdge.
    #[test]
    fn test_participant_events_cache_edge() {
        let size = 20usize;
        let test_size = 11i64;
        let participants = PeerSet::new(vec![
            Peer::new("0xaa", "", ""),
            Peer::new("0xbb", "", ""),
            Peer::new("0xcc", "", ""),
        ]);

        let mut pec = ParticipantEventsCache::new(size);
        for p in &participants.peers {
            pec.add_peer(p).unwrap();
        }

        let pub_keys: Vec<String> = participants.by_pub_key.keys().cloned().collect();
        let mut items: HashMap<String, Vec<String>> = HashMap::new();
        for pk in &pub_keys {
            items.insert(pk.clone(), Vec::new());
        }

        for i in 0..test_size {
            for pk in &pub_keys {
                let item = format!("{}{}", pk, i);
                pec.set(pk, &item, i).unwrap();
                items.get_mut(pk).unwrap().push(item);
            }
        }

        let half = size / 2;
        for pk in &pub_keys {
            let expected = &items[pk][half..];
            let cached = pec.get(pk, half as i64 - 1).unwrap();
            assert_eq!(cached.as_slice(), expected, "expected and cached not equal");
        }
    }

    // Translation of caches_test.go::TestPeerSetCache.
    #[test]
    fn test_peer_set_cache() {
        let mut peer_set_cache = PeerSetCache::new();

        let peer_set0 = Arc::new(PeerSet::new(vec![
            Peer::new("0xaa", "", ""),
            Peer::new("0xbb", "", ""),
            Peer::new("0xcc", "", ""),
        ]));
        peer_set_cache.set(0, peer_set0.clone()).unwrap();

        let peer_set3 = Arc::new(peer_set0.with_new_peer(&Peer::new("0xdd", "", "")));
        peer_set_cache.set(3, peer_set3.clone()).unwrap();

        for i in 0..3 {
            let ps = peer_set_cache.get(i).unwrap();
            assert_eq!(ps.peers, peer_set0.peers, "PeerSet {} mismatch", i);
        }
        for i in 3..6 {
            let ps = peer_set_cache.get(i).unwrap();
            assert_eq!(ps.peers, peer_set3.peers, "PeerSet {} mismatch", i);
        }

        let peer_set2 = Arc::new(peer_set0.with_new_peer(&Peer::new("0xee", "", "")));
        peer_set_cache.set(2, peer_set2.clone()).unwrap();

        let ps2 = peer_set_cache.get(2).unwrap();
        assert_eq!(ps2.peers, peer_set2.peers, "PeerSet 2 mismatch");

        let ps3 = peer_set_cache.get(3).unwrap();
        assert_eq!(ps3.peers, peer_set3.peers, "PeerSet 3 mismatch");

        let err = peer_set_cache
            .set(
                2,
                Arc::new(peer_set2.with_new_peer(&Peer::new("broken", "", ""))),
            )
            .unwrap_err();
        assert!(
            is_store(&err, StoreErrType::KeyAlreadyExists),
            "Resetting PeerSet 2 should throw KeyAlreadyExists"
        );
    }
}
