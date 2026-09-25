//! Translation of `chain/hashgraph/store.go`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::block::Block;
use super::event::Event;
use super::frame::Frame;
use super::root::Root;
use super::round_info::RoundInfo;
use crate::peers::{Peer, PeerSet};

/// An interface for hashgraph backend stores.
///
/// In Go the store handed out `*Event`/`*RoundInfo`/`*Block`/`*Frame` pointers.
/// Events, rounds, blocks and frames have explicit `Set*` methods, so the
/// translation returns them by value (clones) and relies on the caller setting
/// them back — exactly what BadgerStore-backed deployments already require.
/// `Root` has no `SetRoot` method and is mutated through the returned handle,
/// so it is returned behind `Arc<Mutex<_>>`.
pub trait Store: Send {
    /// The cache size limit that determines the maximum number of items caches
    /// can contain.
    fn cache_size(&self) -> i64;
    /// Returns the peer-set effective at a given round.
    fn get_peer_set(&self, round: i64) -> Result<Arc<PeerSet>>;
    /// Sets the peer-set effective at a given round.
    fn set_peer_set(&self, round: i64, peers: Arc<PeerSet>) -> Result<()>;
    /// Returns the entire history of peer-sets.
    fn get_all_peer_sets(&self) -> Result<HashMap<i64, Vec<Peer>>>;
    /// Returns the index of the first round to which a participant belonged.
    fn first_round(&self, participant_id: u32) -> (i64, bool);
    /// Returns all the known peers by public key.
    fn repertoire_by_pub_key(&self) -> HashMap<String, Peer>;
    /// Returns all the known peers by ID.
    fn repertoire_by_id(&self) -> HashMap<u32, Peer>;
    /// Returns an event by hash.
    fn get_event(&self, hash: &str) -> Result<Event>;
    /// Inserts an event into the store.
    fn set_event(&self, event: &Event) -> Result<()>;
    /// Returns all the sorted event hashes of a participant starting at
    /// index `skip + 1`.
    fn participant_events(&self, participant: &str, skip: i64) -> Result<Vec<String>>;
    /// Returns a participant's event with a given index.
    fn participant_event(&self, participant: &str, index: i64) -> Result<String>;
    /// Returns the last event of a participant.
    fn last_event_from(&self, participant: &str) -> Result<String>;
    /// Returns the last consensus event produced by a participant.
    fn last_consensus_event_from(&self, participant: &str) -> Result<String>;
    /// Returns the map of participant ID to last known index.
    fn known_events(&self) -> HashMap<u32, i64>;
    /// Returns the hashes of consensus events.
    fn consensus_events(&self) -> Vec<String>;
    /// Returns the number of consensus events.
    fn consensus_events_count(&self) -> i64;
    /// Adds a consensus event.
    fn add_consensus_event(&self, event: &Event) -> Result<()>;
    /// Retrieves a round by index.
    fn get_round(&self, round_index: i64) -> Result<RoundInfo>;
    /// Stores a round.
    fn set_round(&self, round_index: i64, round_info: &RoundInfo) -> Result<()>;
    /// Returns the index of the last created round.
    fn last_round(&self) -> i64;
    /// Returns the hashes of a round's witnesses.
    fn round_witnesses(&self, round_index: i64) -> Vec<String>;
    /// Returns the number of events in a round.
    fn round_events(&self, round_index: i64) -> i64;
    /// Returns a participant's root.
    fn get_root(&self, participant: &str) -> Result<Arc<Mutex<Root>>>;
    /// Returns a block by index.
    fn get_block(&self, index: i64) -> Result<Block>;
    /// Stores a block.
    fn set_block(&self, block: &Block) -> Result<()>;
    /// Returns the last block index.
    fn last_block_index(&self) -> i64;
    /// Retrieves the frame associated to a round received.
    fn get_frame(&self, round_received: i64) -> Result<Frame>;
    /// Stores a frame.
    fn set_frame(&self, frame: &Frame) -> Result<()>;
    /// Resets a store from a frame.
    fn reset(&self, frame: &Frame) -> Result<()>;
    /// Closes the underlying database.
    fn close(&self) -> Result<()>;
    /// Returns the filepath of the underlying database.
    fn store_path(&self) -> String;
    /// Enables downcasting to a concrete store type (used by `Bootstrap`,
    /// which is RocksDB-store specific).
    fn as_any(&self) -> &dyn std::any::Any;
}
