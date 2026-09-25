//! Translation of `chain/peers/peer_set.go`.

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::Result;

use super::peer::Peer;
use crate::common;
use crate::crypto;

/// Represents a collection of peers.
///
/// Go stored peers as `*Peer` shared between the slice and the two maps; since
/// `Peer` is an immutable value type here, the maps hold clones. The lazily
/// computed caches use `OnceLock` instead of mutable cache fields.
#[derive(Debug)]
pub struct PeerSet {
    pub peers: Vec<Peer>,
    pub by_pub_key: HashMap<String, Peer>,
    pub by_id: HashMap<u32, Peer>,

    // cached values
    hash: OnceLock<Vec<u8>>,
    hex: OnceLock<String>,
    super_majority: OnceLock<i64>,
    trust_count: OnceLock<i64>,
}

impl PeerSet {
    /// Creates a new `PeerSet` from a list of Peers.
    pub fn new(peers: Vec<Peer>) -> PeerSet {
        let mut peer_set = PeerSet {
            peers,
            by_pub_key: HashMap::new(),
            by_id: HashMap::new(),
            hash: OnceLock::new(),
            hex: OnceLock::new(),
            super_majority: OnceLock::new(),
            trust_count: OnceLock::new(),
        };
        peer_set.init_maps();
        peer_set
    }

    fn init_maps(&mut self) {
        self.by_pub_key.clear();
        self.by_id.clear();
        for peer in &self.peers {
            self.by_pub_key.insert(peer.pub_key_string(), peer.clone());
            self.by_id.insert(peer.id(), peer.clone());
        }
    }

    /// Returns a new `PeerSet` including the new peer.
    pub fn with_new_peer(&self, peer: &Peer) -> PeerSet {
        let mut peers = self.peers.clone();
        // don't add it if it already exists
        if !self.by_id.contains_key(&peer.id()) {
            peers.push(peer.clone());
        }
        PeerSet::new(peers)
    }

    /// Returns a new `PeerSet` excluding the provided peer.
    pub fn with_removed_peer(&self, peer: &Peer) -> PeerSet {
        let peers: Vec<Peer> = self
            .peers
            .iter()
            .filter(|p| p.pub_key_hex != peer.pub_key_hex)
            .cloned()
            .collect();
        PeerSet::new(peers)
    }

    /// Returns the PeerSet's slice of public keys.
    pub fn pub_keys(&self) -> Vec<String> {
        self.peers.iter().map(|p| p.pub_key_string()).collect()
    }

    /// Returns the PeerSet's slice of IDs.
    pub fn ids(&self) -> Vec<u32> {
        self.peers.iter().map(|p| p.id()).collect()
    }

    /// Returns the number of Peers in the PeerSet.
    pub fn len(&self) -> i64 {
        self.by_pub_key.len() as i64
    }

    pub fn is_empty(&self) -> bool {
        self.by_pub_key.is_empty()
    }

    /// Uniquely identifies a PeerSet, by hashing their public keys together.
    pub fn hash(&self) -> Vec<u8> {
        self.hash
            .get_or_init(|| {
                let mut hash: Vec<u8> = Vec::new();
                for p in &self.peers {
                    let pk = p.pub_key_bytes();
                    hash = crypto::simple_hash_from_two_hashes(&hash, &pk);
                }
                hash
            })
            .clone()
    }

    /// The hexadecimal representation of [`PeerSet::hash`].
    pub fn hex(&self) -> String {
        self.hex
            .get_or_init(|| common::encode_to_string(&self.hash()))
            .clone()
    }

    /// Marshals the PeerSet (its peer slice).
    pub fn marshal(&self) -> Result<Vec<u8>> {
        // Go's `json.Encoder.Encode` appends a trailing newline.
        let mut buf = serde_json::to_vec(&self.peers)?;
        buf.push(b'\n');
        Ok(buf)
    }

    /// Unmarshals a PeerSet. Go's `Unmarshal` mutated the receiver; the
    /// translation is an associated constructor.
    pub fn unmarshal(peer_slice_bytes: &[u8]) -> Result<PeerSet> {
        let peers: Vec<Peer> = serde_json::from_slice(peer_slice_bytes)?;
        Ok(PeerSet::new(peers))
    }

    /// The number of peers that forms a strong majority (+2/3) in the PeerSet.
    pub fn super_majority(&self) -> i64 {
        *self.super_majority.get_or_init(|| 2 * self.len() / 3 + 1)
    }

    /// The minimum number of signatures that may represent the finality of a
    /// state, given the assumptions of the consensus algorithm.
    pub fn trust_count(&self) -> i64 {
        *self.trust_count.get_or_init(|| {
            if self.peers.len() > 1 {
                (self.len() as f64 / 3.0).ceil() as i64
            } else {
                0
            }
        })
    }

    /// Clears the cached `hash`, `hex` and `super_majority` values. As in Go,
    /// `trust_count` is intentionally left untouched.
    pub fn clear_cache(&mut self) {
        self.hash = OnceLock::new();
        self.hex = OnceLock::new();
        self.super_majority = OnceLock::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::keys::{generate_ecdsa_key, public_key_hex};

    fn make_peer(addr: &str, moniker: &str) -> Peer {
        let key = generate_ecdsa_key().unwrap();
        Peer::new(&public_key_hex(key.verifying_key()), addr, moniker)
    }

    fn make_peers(n: usize) -> Vec<Peer> {
        (0..n)
            .map(|i| make_peer(&format!("a{}", i), &format!("m{}", i)))
            .collect()
    }

    #[test]
    fn new_initialises_lookup_maps() {
        let peers = make_peers(3);
        let ps = PeerSet::new(peers.clone());
        assert_eq!(ps.len(), 3);
        for p in &peers {
            assert!(ps.by_pub_key.contains_key(&p.pub_key_string()));
            assert!(ps.by_id.contains_key(&p.id()));
        }
    }

    #[test]
    fn with_new_peer_adds_and_with_removed_peer_removes() {
        let peers = make_peers(3);
        let ps = PeerSet::new(peers.clone());
        let extra = make_peer("a3", "m3");

        let added = ps.with_new_peer(&extra);
        assert_eq!(added.len(), 4);
        assert!(added.by_id.contains_key(&extra.id()));

        let removed = added.with_removed_peer(&extra);
        assert_eq!(removed.len(), 3);
        assert!(!removed.by_id.contains_key(&extra.id()));
    }

    #[test]
    fn with_new_peer_ignores_duplicate() {
        let peers = make_peers(2);
        let ps = PeerSet::new(peers.clone());
        let dup = ps.with_new_peer(&peers[0]);
        assert_eq!(dup.len(), 2, "duplicate add should be a no-op");
    }

    #[test]
    fn empty_peer_set_is_empty() {
        let ps = PeerSet::new(Vec::new());
        assert!(ps.is_empty());
        assert_eq!(ps.len(), 0);
    }

    #[test]
    fn hash_is_cached_and_deterministic_independent_of_order_via_caches() {
        let peers = make_peers(4);
        let ps = PeerSet::new(peers.clone());
        let h1 = ps.hash();
        let h2 = ps.hash();
        assert_eq!(h1, h2);
        // hex should be the hex-encoded form of the same hash.
        assert_eq!(ps.hex(), common::encode_to_string(&h1));
    }

    #[test]
    fn hash_changes_with_membership() {
        let peers = make_peers(3);
        let ps = PeerSet::new(peers.clone());
        let extra = make_peer("addr_extra", "extra");
        let bigger = ps.with_new_peer(&extra);
        assert_ne!(ps.hash(), bigger.hash());
    }

    #[test]
    fn super_majority_and_trust_count() {
        // 1 peer: super_majority = 2*1/3 + 1 = 1; trust_count = 0 (single-node guard)
        let ps1 = PeerSet::new(make_peers(1));
        assert_eq!(ps1.super_majority(), 1);
        assert_eq!(ps1.trust_count(), 0);

        // 4 peers: super_majority = 2*4/3 + 1 = 3; trust_count = ceil(4/3) = 2
        let ps4 = PeerSet::new(make_peers(4));
        assert_eq!(ps4.super_majority(), 3);
        assert_eq!(ps4.trust_count(), 2);

        // 7 peers: super_majority = 2*7/3 + 1 = 5; trust_count = ceil(7/3) = 3
        let ps7 = PeerSet::new(make_peers(7));
        assert_eq!(ps7.super_majority(), 5);
        assert_eq!(ps7.trust_count(), 3);
    }

    #[test]
    fn pub_keys_and_ids_mirror_inserted_order() {
        let peers = make_peers(3);
        let ps = PeerSet::new(peers.clone());
        let keys: Vec<String> = peers.iter().map(|p| p.pub_key_string()).collect();
        let ids: Vec<u32> = peers.iter().map(|p| p.id()).collect();
        assert_eq!(ps.pub_keys(), keys);
        assert_eq!(ps.ids(), ids);
    }

    #[test]
    fn marshal_unmarshal_round_trip() {
        let ps = PeerSet::new(make_peers(3));
        let bytes = ps.marshal().expect("marshal");
        assert_eq!(*bytes.last().unwrap(), b'\n');
        let restored = PeerSet::unmarshal(&bytes).expect("unmarshal");
        assert_eq!(restored.len(), ps.len());
        // Restored peer set has the same membership hash.
        assert_eq!(restored.hash(), ps.hash());
    }

    #[test]
    fn clear_cache_resets_hash_but_not_trust_count() {
        let mut ps = PeerSet::new(make_peers(4));
        let original_hash = ps.hash();
        let original_trust = ps.trust_count();
        ps.clear_cache();
        // Recomputed hash must still match (input is unchanged).
        assert_eq!(ps.hash(), original_hash);
        // trust_count cache is intentionally retained.
        assert_eq!(ps.trust_count(), original_trust);
    }
}
