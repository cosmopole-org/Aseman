//! Translation of `chain/node/peer_selector.go`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::drivers::network::chain::peers::{exclude_peer, Peer, PeerSet};

/// Selects the next gossip peer based on a list of peers.
pub trait PeerSelector: Send {
    fn get_peers(&self) -> Arc<PeerSet>;
    fn update_last(&mut self, peer: u32, connected: bool) -> bool;
    fn next(&self) -> Option<Peer>;
}

/// A wrapper around a Peer that tracks its connection status.
struct PeerSelectorItem {
    peer: Peer,
    connected: bool,
}

/// Selects the next peer at random, tracking each peer's connection status.
pub struct RandomPeerSelector {
    peers: Arc<PeerSet>,
    self_id: u32,
    selectable_peers_map: HashMap<u32, PeerSelectorItem>,
    selectable_peers_slice: Vec<u32>,
    last: u32,
}

impl RandomPeerSelector {
    /// Creates a new `RandomPeerSelector`, excluding the peer identified by
    /// `self_id`.
    pub fn new(peer_set: Arc<PeerSet>, self_id: u32) -> RandomPeerSelector {
        let (_, other_peers) = exclude_peer(&peer_set.peers, self_id);

        let mut selectable_peers_map = HashMap::new();
        let mut selectable_peers_slice = Vec::new();
        for p in other_peers {
            let id = p.id();
            selectable_peers_map.insert(
                id,
                PeerSelectorItem {
                    peer: p,
                    connected: false,
                },
            );
            selectable_peers_slice.push(id);
        }

        RandomPeerSelector {
            peers: peer_set,
            self_id,
            selectable_peers_map,
            selectable_peers_slice,
            last: 0,
        }
    }
}

impl PeerSelector for RandomPeerSelector {
    fn get_peers(&self) -> Arc<PeerSet> {
        self.peers.clone()
    }

    fn update_last(&mut self, peer: u32, connected: bool) -> bool {
        self.last = peer;

        // The peer could have been removed by an InternalTransaction.
        if let Some(item) = self.selectable_peers_map.get_mut(&peer) {
            let old = item.connected;
            item.connected = connected;
            if !old && connected {
                return true;
            }
        }
        false
    }

    fn next(&self) -> Option<Peer> {
        if self.selectable_peers_slice.is_empty() {
            return None;
        }

        let mut next_id = self.selectable_peers_slice[0];

        if self.selectable_peers_slice.len() > 1 {
            let other_peers: Vec<u32> = self
                .selectable_peers_slice
                .iter()
                .copied()
                .filter(|pid| *pid != self.last)
                .collect();
            let i = rand::random::<u64>() as usize % other_peers.len();
            next_id = other_peers[i];
        }

        self.selectable_peers_map
            .get(&next_id)
            .map(|it| it.peer.clone())
    }
}
