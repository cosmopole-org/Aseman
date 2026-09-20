//! Translation of `chain/hashgraph/badger_store.go`.
//!
//! The Go node persisted the hashgraph in a Badger key-value database. As
//! requested, the translation uses a **RocksDB** instance instead (a separate
//! database, co-located with the appengine's RocksDB). The struct is named
//! `RocksDbStore`; its behaviour — an `InmemStore` cache in front of an
//! on-disk key-value store — is otherwise a faithful translation of
//! `BadgerStore`.
//!
//! The `mobile`-tagged `badger_store_mobile.go` was a near-duplicate behind a
//! build tag (with stale `babble/...` import paths); it is not translated
//! separately.

use std::cell::Cell;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use rocksdb::{Direction, IteratorMode, Options, WriteBatch, DB};

use super::block::Block;
use super::event::Event;
use super::frame::Frame;
use super::inmem_store::InmemStore;
use super::root::Root;
use super::round_info::RoundInfo;
use super::store::Store;
use crate::drivers::network::chain::common::{new_store_err, StoreErrType};
use crate::drivers::network::chain::peers::{Peer, PeerSet};

const REPERTOIRE_PREFIX: &str = "rep";
const PEER_SET_PREFIX: &str = "peerset";
const ROOT_SUFFIX: &str = "root";
const ROUND_PREFIX: &str = "round";
const TOPO_PREFIX: &str = "topo";
const BLOCK_PREFIX: &str = "block";
const FRAME_PREFIX: &str = "frame";

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

fn repertoire_key(pub_: &str) -> String {
    format!("{}_{}", REPERTOIRE_PREFIX, pub_)
}
fn peer_set_key(round: i64) -> String {
    format!("{}_{:09}", PEER_SET_PREFIX, round)
}
fn topological_event_key(index: i64) -> String {
    format!("{}_{:09}", TOPO_PREFIX, index)
}
fn participant_event_key(participant: &str, index: i64) -> String {
    format!("{}__event_{:09}", participant, index)
}
fn participant_root_key(participant: &str) -> String {
    format!("{}_{}", participant, ROOT_SUFFIX)
}
fn round_key(index: i64) -> String {
    format!("{}_{:09}", ROUND_PREFIX, index)
}
fn block_key(index: i64) -> String {
    format!("{}_{:09}", BLOCK_PREFIX, index)
}
fn frame_key(index: i64) -> String {
    format!("{}_{:09}", FRAME_PREFIX, index)
}

/// How many recent frames to keep on disk. Babble writes a frame per decided
/// round and never removes them, so the store otherwise grows without bound
/// (each frame is a full round snapshot — the single biggest consumer, and the
/// main driver of the data-dir bloat that fills the box). A pruned frame is
/// recomputed from the retained events/rounds on the rare miss (see
/// `Hashgraph::get_frame`), so trimming old ones is safe. The last 25 rounds
/// by default, env-tunable through `CASPAR_BABBLE_FRAME_RETENTION`.
const DEFAULT_FRAME_RETENTION_ROUNDS: i64 = 25;

fn frame_retention_rounds() -> i64 {
    aseman_config::legacy_adapter_snapshot()
        .map(|config| config.babble_frame_retention)
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_FRAME_RETENTION_ROUNDS)
}

/// Prune frames older than the retention window at most every this many
/// rounds — a range delete is one tombstone, so an infrequent cadence keeps
/// read amplification negligible while still cleaning up any existing backlog.
const FRAME_PRUNE_EVERY_ROUNDS: i64 = 128;

/// A short retention window is pruned as often as it is long, so the disk
/// never holds more than about twice the window.
fn frame_prune_every_rounds(retention: i64) -> i64 {
    retention.clamp(1, FRAME_PRUNE_EVERY_ROUNDS)
}

/// Contains references to the RocksDB database and the in-memory store. When
/// `maintenance_mode` is active, data is written only to the caches.
pub struct RocksDbStore {
    inmem_store: InmemStore,
    db: DB,
    path: String,
    maintenance_mode: Cell<bool>,
}

impl RocksDbStore {
    /// Opens an existing database or creates a new one at `path`. The
    /// `maintenance_mode` option deactivates writing to the persistent
    /// database, while still adding/updating the in-memory store.
    pub fn new(cache_size: i64, path: &str, maintenance_mode: bool) -> Result<RocksDbStore> {
        // Bounded-memory options (capped open files + shared LRU block cache):
        // the consensus DB gains an SST file on every decided round and is
        // never pruned, so the default unlimited `max_open_files` pinned an
        // ever-growing set of index/filter blocks in RAM. See
        // `crate::drivers::rocks_tuning`.
        let mut opts = crate::drivers::rocks_tuning::tuned_options();
        opts.create_if_missing(true);
        let db = DB::open(&opts, path)?;

        Ok(RocksDbStore {
            inmem_store: InmemStore::new(cache_size),
            db,
            path: path.to_string(),
            maintenance_mode: Cell::new(maintenance_mode),
        })
    }

    /// Getter for the maintenance-mode flag.
    pub fn get_maintenance_mode(&self) -> bool {
        self.maintenance_mode.get()
    }

    /// Setter for the maintenance-mode flag.
    pub fn set_maintenance_mode(&self, val: bool) {
        self.maintenance_mode.set(val);
    }

    fn add_participant(&self, p: &Peer) -> Result<()> {
        if self.maintenance_mode.get() {
            return Ok(());
        }
        self.db_set_repertoire(p)?;
        if self.db_get_root(&p.pub_key_string()).is_err() {
            let root = Root::new();
            self.db_set_root(&p.pub_key_string(), &root)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // DB methods
    // -----------------------------------------------------------------------

    fn db_get_repertoire(&self) -> Result<std::collections::HashMap<String, Peer>> {
        let mut repertoire = std::collections::HashMap::new();
        let prefix = REPERTOIRE_PREFIX.as_bytes();
        let iter = self
            .db
            .iterator(IteratorMode::From(prefix, Direction::Forward));
        for item in iter {
            let (k, v) = item?;
            if !k.starts_with(prefix) {
                break;
            }
            let mut peer = Peer::default();
            peer.unmarshal(&v)?;
            repertoire.insert(peer.pub_key_string(), peer);
        }
        Ok(repertoire)
    }

    fn db_set_repertoire(&self, peer: &Peer) -> Result<()> {
        let key = repertoire_key(&peer.pub_key_string());
        let val = peer.marshal()?;
        self.db.put(key.as_bytes(), val)?;
        Ok(())
    }

    pub(crate) fn db_get_peer_set(&self, round: i64) -> Result<PeerSet> {
        let key = peer_set_key(round);
        match self.db.get(key.as_bytes())? {
            Some(bytes) => PeerSet::unmarshal(&bytes),
            None => Err(new_store_err("PeerSet", StoreErrType::KeyNotFound, &key).into()),
        }
    }

    fn db_set_peer_set(&self, round: i64, peer_set: &PeerSet) -> Result<()> {
        let key = peer_set_key(round);
        let val = peer_set.marshal()?;
        self.db.put(key.as_bytes(), val)?;
        Ok(())
    }

    fn db_get_event(&self, key: &str) -> Result<Event> {
        match self.db.get(key.as_bytes())? {
            Some(bytes) => {
                let mut event = Event::default();
                event.unmarshal_db(&bytes)?;
                Ok(event)
            }
            None => Err(new_store_err("Event", StoreErrType::KeyNotFound, key).into()),
        }
    }

    fn db_set_events(&self, events: &[&Event]) -> Result<()> {
        let mut batch = WriteBatch::default();
        for event in events {
            let event_hex = event.hex();
            let val = event.marshal_db()?;
            // check if it already exists
            let is_new = self.db.get(event_hex.as_bytes())?.is_none();
            batch.put(event_hex.as_bytes(), &val);
            if is_new {
                let topo_key = topological_event_key(event.topological_index);
                batch.put(topo_key.as_bytes(), event_hex.as_bytes());
                let pe_key = participant_event_key(&event.creator(), event.index());
                batch.put(pe_key.as_bytes(), event_hex.as_bytes());
            }
        }
        self.db.write(batch)?;
        Ok(())
    }

    fn db_participant_events(&self, participant: &str, skip: i64) -> Result<Vec<String>> {
        let mut res = Vec::new();
        let mut i = skip + 1;
        loop {
            let key = participant_event_key(participant, i);
            match self.db.get(key.as_bytes())? {
                Some(v) => {
                    res.push(String::from_utf8_lossy(&v).into_owned());
                    i += 1;
                }
                None => break,
            }
        }
        Ok(res)
    }

    fn db_participant_event(&self, participant: &str, index: i64) -> Result<String> {
        let key = participant_event_key(participant, index);
        match self.db.get(key.as_bytes())? {
            Some(v) => Ok(String::from_utf8_lossy(&v).into_owned()),
            None => Err(new_store_err("Event", StoreErrType::KeyNotFound, &key).into()),
        }
    }

    pub(crate) fn db_topological_events(&self, start: i64, count: i64) -> Result<Vec<Event>> {
        let mut res = Vec::new();
        let mut t = start;
        while t < start + count {
            let key = topological_event_key(t);
            let ev_key = match self.db.get(key.as_bytes())? {
                Some(v) => v,
                None => break,
            };
            let event_bytes = self
                .db
                .get(&ev_key)?
                .ok_or_else(|| anyhow!("topological event hash not found"))?;
            let mut event = Event::default();
            event.unmarshal_db(&event_bytes)?;
            res.push(event);
            t += 1;
        }
        Ok(res)
    }

    fn db_set_root(&self, participant: &str, root: &Root) -> Result<()> {
        let key = participant_root_key(participant);
        let val = root.marshal()?;
        self.db.put(key.as_bytes(), val)?;
        Ok(())
    }

    fn db_get_root(&self, participant: &str) -> Result<Root> {
        let key = participant_root_key(participant);
        match self.db.get(key.as_bytes())? {
            Some(bytes) => {
                let mut root = Root::new();
                root.unmarshal(&bytes)?;
                Ok(root)
            }
            None => Err(new_store_err("Root", StoreErrType::KeyNotFound, &key).into()),
        }
    }

    fn db_get_round(&self, index: i64) -> Result<RoundInfo> {
        let key = round_key(index);
        match self.db.get(key.as_bytes())? {
            Some(bytes) => {
                let mut round_info = RoundInfo::new();
                round_info.unmarshal(&bytes)?;
                Ok(round_info)
            }
            None => Err(new_store_err("Round", StoreErrType::KeyNotFound, &key).into()),
        }
    }

    fn db_set_round(&self, index: i64, round: &RoundInfo) -> Result<()> {
        let key = round_key(index);
        let val = round.marshal()?;
        self.db.put(key.as_bytes(), val)?;
        Ok(())
    }

    fn db_get_block(&self, index: i64) -> Result<Block> {
        let key = block_key(index);
        match self.db.get(key.as_bytes())? {
            Some(bytes) => {
                let mut block = Block::new(0, 0, vec![], vec![], vec![], vec![], 0);
                block.unmarshal(&bytes)?;
                Ok(block)
            }
            None => Err(new_store_err("Block", StoreErrType::KeyNotFound, &key).into()),
        }
    }

    fn db_set_block(&self, block: &Block) -> Result<()> {
        let key = block_key(block.index());
        let val = block.marshal()?;
        self.db.put(key.as_bytes(), val)?;
        Ok(())
    }

    fn db_get_frame(&self, index: i64) -> Result<Frame> {
        let key = frame_key(index);
        match self.db.get(key.as_bytes())? {
            Some(bytes) => {
                let mut frame = Frame::default();
                frame.unmarshal(&bytes)?;
                Ok(frame)
            }
            None => Err(new_store_err("Frame", StoreErrType::KeyNotFound, &key).into()),
        }
    }

    fn db_set_frame(&self, frame: &Frame) -> Result<()> {
        let key = frame_key(frame.round);
        let val = frame.marshal()?;
        self.db.put(key.as_bytes(), val)?;
        // Bound on-disk frame growth: periodically range-delete frames older than
        // the retention window. `frame_key` is zero-padded and `frame` sorts
        // between `block` and `round`, so this range only ever contains frame
        // keys with round < cutoff. Range-delete cleans the whole backlog (not
        // just one round), which is what reclaims a data-dir already bloated by
        // old frames. Pruned frames recompute on the rare miss.
        let retention = frame_retention_rounds();
        if frame.round > 0 && frame.round % frame_prune_every_rounds(retention) == 0 {
            let cutoff = frame.round - retention;
            if cutoff > 0 {
                let mut batch = WriteBatch::default();
                batch.delete_range(frame_key(0).as_bytes(), frame_key(cutoff).as_bytes());
                let _ = self.db.write(batch);
            }
        }
        Ok(())
    }
}

impl Store for RocksDbStore {
    fn cache_size(&self) -> i64 {
        self.inmem_store.cache_size()
    }

    // ---- Cache-only methods ----

    fn get_round(&self, r: i64) -> Result<RoundInfo> {
        self.inmem_store.get_round(r)
    }

    fn round_witnesses(&self, r: i64) -> Vec<String> {
        match self.inmem_store.get_round(r) {
            Ok(round) => round.witnesses(),
            Err(_) => Vec::new(),
        }
    }

    fn round_events(&self, r: i64) -> i64 {
        match self.inmem_store.get_round(r) {
            Ok(round) => round.created_events.len() as i64,
            Err(_) => 0,
        }
    }

    fn get_frame(&self, rr: i64) -> Result<Frame> {
        // Frames are held in a small in-memory hot set (see
        // `inmem_store::max_frame_cache`); on a miss fall back to the durable
        // RocksDB copy written by `db_set_frame`. Without this fallback, evicting
        // a frame from the cache would make it unreadable even though it is on
        // disk. The DB read is not re-cached, so the hot set stays bounded.
        match self.inmem_store.get_frame(rr) {
            Ok(frame) => Ok(frame),
            Err(_) => self.db_get_frame(rr),
        }
    }

    fn get_peer_set(&self, round: i64) -> Result<Arc<PeerSet>> {
        self.inmem_store.get_peer_set(round)
    }

    fn get_all_peer_sets(&self) -> Result<std::collections::HashMap<i64, Vec<Peer>>> {
        self.inmem_store.get_all_peer_sets()
    }

    fn first_round(&self, id: u32) -> (i64, bool) {
        self.inmem_store.first_round(id)
    }

    fn repertoire_by_pub_key(&self) -> std::collections::HashMap<String, Peer> {
        self.inmem_store.repertoire_by_pub_key()
    }

    fn repertoire_by_id(&self) -> std::collections::HashMap<u32, Peer> {
        self.inmem_store.repertoire_by_id()
    }

    fn last_event_from(&self, participant: &str) -> Result<String> {
        self.inmem_store.last_event_from(participant)
    }

    fn last_consensus_event_from(&self, participant: &str) -> Result<String> {
        self.inmem_store.last_consensus_event_from(participant)
    }

    fn known_events(&self) -> std::collections::HashMap<u32, i64> {
        self.inmem_store.known_events()
    }

    fn consensus_events(&self) -> Vec<String> {
        self.inmem_store.consensus_events()
    }

    fn consensus_events_count(&self) -> i64 {
        self.inmem_store.consensus_events_count()
    }

    fn add_consensus_event(&self, event: &Event) -> Result<()> {
        self.inmem_store.add_consensus_event(event)
    }

    fn last_round(&self) -> i64 {
        self.inmem_store.last_round()
    }

    fn last_block_index(&self) -> i64 {
        self.inmem_store.last_block_index()
    }

    // ---- Cache + DB methods ----

    fn set_peer_set(&self, round: i64, peer_set: Arc<PeerSet>) -> Result<()> {
        self.inmem_store.set_peer_set(round, peer_set.clone())?;

        if !self.maintenance_mode.get() {
            self.db_set_peer_set(round, &peer_set)?;
        }

        for p in &peer_set.peers {
            self.add_participant(p)?;
        }
        Ok(())
    }

    fn set_event(&self, event: &Event) -> Result<()> {
        self.inmem_store.set_event(event)?;
        if self.maintenance_mode.get() {
            return Ok(());
        }
        self.db_set_events(&[event])
    }

    fn participant_events(&self, participant: &str, skip: i64) -> Result<Vec<String>> {
        match self.inmem_store.participant_events(participant, skip) {
            Ok(res) => Ok(res),
            Err(_) => self.db_participant_events(participant, skip),
        }
    }

    fn participant_event(&self, participant: &str, index: i64) -> Result<String> {
        match self.inmem_store.participant_event(participant, index) {
            Ok(res) => Ok(res),
            Err(_) => self.db_participant_event(participant, index),
        }
    }

    fn set_round(&self, r: i64, round: &RoundInfo) -> Result<()> {
        self.inmem_store.set_round(r, round)?;
        if self.maintenance_mode.get() {
            return Ok(());
        }
        self.db_set_round(r, round)
    }

    fn get_root(&self, participant: &str) -> Result<Arc<Mutex<Root>>> {
        match self.inmem_store.get_root(participant) {
            Ok(root) => Ok(root),
            Err(_) => {
                let root = self.db_get_root(participant)?;
                Ok(Arc::new(Mutex::new(root)))
            }
        }
    }

    fn get_event(&self, key: &str) -> Result<Event> {
        match self.inmem_store.get_event(key) {
            Ok(ev) => Ok(ev),
            Err(_) => self.db_get_event(key),
        }
    }

    fn get_block(&self, rr: i64) -> Result<Block> {
        match self.inmem_store.get_block(rr) {
            Ok(res) => Ok(res),
            Err(_) => self.db_get_block(rr),
        }
    }

    fn set_block(&self, block: &Block) -> Result<()> {
        self.inmem_store.set_block(block)?;
        if self.maintenance_mode.get() {
            return Ok(());
        }
        self.db_set_block(block)
    }

    fn set_frame(&self, frame: &Frame) -> Result<()> {
        self.inmem_store.set_frame(frame)?;
        if self.maintenance_mode.get() {
            return Ok(());
        }
        self.db_set_frame(frame)
    }

    fn reset(&self, frame: &Frame) -> Result<()> {
        self.inmem_store.reset(frame)?;

        if self.maintenance_mode.get() {
            return Ok(());
        }

        self.db_set_frame(frame)?;

        for (p, root) in &frame.roots {
            self.db_set_root(p, root)?;
        }

        let peer_set = PeerSet::new(frame.peers.clone());
        self.db_set_peer_set(frame.round, &peer_set)?;

        Ok(())
    }

    fn close(&self) -> Result<()> {
        self.inmem_store.close()?;
        let _ = self.db.flush();
        Ok(())
    }

    fn store_path(&self) -> String {
        self.path.clone()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::network::chain::hashgraph::block::{Block, BlockSignature};
    use crate::drivers::network::chain::hashgraph::event::{Event, FrameEvent};
    use crate::drivers::network::chain::hashgraph::frame::Frame;
    use crate::drivers::network::chain::hashgraph::inmem_store::test_helpers::init_peers;
    use crate::drivers::network::chain::hashgraph::internal_transaction::{
        InternalTransaction, TransactionType,
    };
    use crate::drivers::network::chain::hashgraph::round_info::RoundInfo;
    use crate::drivers::network::chain::peers::Peer;
    use std::collections::{BTreeMap, HashMap};

    fn temp_store(cache_size: i64) -> (RocksDbStore, std::path::PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("babble-rocks-{}-{}", std::process::id(), nanos));
        let store = RocksDbStore::new(cache_size, dir.to_str().unwrap(), false).unwrap();
        (store, dir)
    }

    fn remove_store(store: RocksDbStore, dir: &std::path::PathBuf) {
        store.close().unwrap();
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    // Translation of badger_store_test.go::TestNewBadgerStore.
    #[test]
    fn test_new_rocks_store() {
        let (store, dir) = temp_store(1000);
        assert!(std::path::Path::new(&store.path).exists());
        remove_store(store, &dir);
    }

    // Translation of badger_store_test.go::TestDBRepertoireMethods.
    #[test]
    fn test_db_repertoire_methods() {
        let (store, dir) = temp_store(0);
        let (peer_set, _) = init_peers(3);

        for p in &peer_set.peers {
            store.db_set_repertoire(p).unwrap();
        }
        let repertoire = store.db_get_repertoire().unwrap();
        assert_eq!(repertoire, peer_set.by_pub_key, "Repertoire mismatch");

        remove_store(store, &dir);
    }

    // Translation of badger_store_test.go::TestDBPeerSetMethods.
    #[test]
    fn test_db_peer_set_methods() {
        let (store, dir) = temp_store(0);
        let (peer_set, _) = init_peers(3);

        store.db_set_peer_set(0, &peer_set).unwrap();
        let peer_set0 = store.db_get_peer_set(0).unwrap();
        assert_eq!(
            peer_set0.peers, peer_set.peers,
            "Retrieved PeerSet mismatch"
        );

        remove_store(store, &dir);
    }

    // Translation of badger_store_test.go::TestDBEventMethods.
    #[test]
    fn test_db_event_methods() {
        let (store, dir) = temp_store(0);
        let test_size = 100i64;
        let (_, participants) = init_peers(3);

        let mut events: HashMap<String, Vec<Event>> = HashMap::new();
        let mut topological_index = 0i64;
        let mut topological_events: Vec<Event> = Vec::new();
        for p in &participants {
            let mut items: Vec<Event> = Vec::new();
            for k in 0..test_size {
                let mut event = Event::new(
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
                event.sign(&p.priv_key).unwrap();
                event.topological_index = topological_index;
                topological_index += 1;
                store.db_set_events(&[&event]).unwrap();
                topological_events.push(event.clone());
                items.push(event);
            }
            events.insert(p.hex.clone(), items);
        }

        for (_p, evs) in &events {
            for ev in evs {
                let rev = store.db_get_event(&ev.hex()).unwrap();
                assert_eq!(ev.body, rev.body, "event body mismatch");
                assert_eq!(ev.signature, rev.signature, "event signature mismatch");
                assert!(rev.verify().unwrap_or(false), "failed to verify signature");
            }
        }

        // check topological order
        let mut db_topological_events: Vec<Event> = Vec::new();
        let mut index = 0i64;
        let batch_size = 100i64;
        loop {
            let batch = store
                .db_topological_events(index * batch_size, batch_size)
                .unwrap();
            let n = batch.len() as i64;
            db_topological_events.extend(batch);
            if n < batch_size {
                break;
            }
            index += 1;
        }

        assert_eq!(
            db_topological_events.len(),
            topological_events.len(),
            "topological events length mismatch"
        );
        for (i, dte) in db_topological_events.iter().enumerate() {
            let te = &topological_events[i];
            assert_eq!(dte.hex(), te.hex(), "topological hex mismatch");
            assert_eq!(te.body, dte.body, "topological body mismatch");
            assert_eq!(
                te.signature, dte.signature,
                "topological signature mismatch"
            );
            assert!(dte.verify().unwrap_or(false), "failed to verify signature");
        }

        remove_store(store, &dir);
    }

    // Translation of badger_store_test.go::TestDBRoundMethods.
    #[test]
    fn test_db_round_methods() {
        let (store, dir) = temp_store(0);
        let (_, participants) = init_peers(3);

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

        store.db_set_round(0, &round).unwrap();
        let stored_round = store.db_get_round(0).unwrap();
        assert_eq!(round, stored_round, "Round and StoredRound do not match");

        remove_store(store, &dir);
    }

    fn make_block(participants: &[super::super::inmem_store::test_helpers::Participant]) -> Block {
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
                Peer::new("peer1", "paris", "peer1"),
            ),
            InternalTransaction::new(
                TransactionType::PeerRemove,
                Peer::new("peer2", "london", "peer2"),
            ),
        ];
        let mut block = Block::new(
            0,
            5,
            b"this is the frame hash".to_vec(),
            vec![],
            transactions,
            internal_transactions,
            0,
        );
        let receipts: Vec<_> = block
            .internal_transactions()
            .iter()
            .map(|itx| itx.as_accepted())
            .collect();
        block.body.internal_transaction_receipts = receipts;
        let sig1 = block.sign(&participants[0].priv_key).unwrap();
        let sig2 = block.sign(&participants[1].priv_key).unwrap();
        block.set_signature(sig1).unwrap();
        block.set_signature(sig2).unwrap();
        block
    }

    // Translation of badger_store_test.go::TestDBBlockMethods.
    #[test]
    fn test_db_block_methods() {
        let (store, dir) = temp_store(0);
        let (_, participants) = init_peers(3);
        let block = make_block(&participants);

        store.db_set_block(&block).unwrap();
        let stored_block = store.db_get_block(0).unwrap();
        assert_eq!(stored_block.body, block.body, "Block bodies mismatch");
        assert_eq!(
            stored_block.signatures, block.signatures,
            "Block signatures mismatch"
        );

        remove_store(store, &dir);
    }

    // Translation of badger_store_test.go::TestDBFrameMethods.
    #[test]
    fn test_db_frame_methods() {
        let (store, dir) = temp_store(0);
        let (peer_set, participants) = init_peers(3);

        let mut events: Vec<FrameEvent> = Vec::new();
        let mut roots: BTreeMap<String, Root> = BTreeMap::new();
        for p in &participants {
            let mut event = Event::new(
                vec![format!("{}_{}", &p.hex[..5], 0).into_bytes()],
                vec![],
                vec![BlockSignature {
                    validator: b"validator".to_vec(),
                    index: 0,
                    signature: "r|s".to_string(),
                }],
                vec![String::new(), String::new()],
                p.pub_key.clone(),
                0,
            );
            event.sign(&p.priv_key).unwrap();
            events.push(FrameEvent {
                core: Box::new(event),
                round: 1,
                lamport_timestamp: 1,
                witness: true,
            });
            roots.insert(p.hex.clone(), Root::new());
        }

        let frame = Frame {
            round: 1,
            peers: peer_set.peers.clone(),
            events,
            roots,
            ..Frame::default()
        };

        store.db_set_frame(&frame).unwrap();
        let stored_frame = store.db_get_frame(frame.round).unwrap();
        assert_eq!(stored_frame, frame, "Frame and StoredFrame do not match");

        remove_store(store, &dir);
    }

    // Translation of badger_store_test.go::TestBadgerPeerSets.
    #[test]
    fn test_rocks_peer_sets() {
        let (store, dir) = temp_store(1000);
        let (peer_set, _) = init_peers(3);
        let peer_set = Arc::new(peer_set);

        store.set_peer_set(0, peer_set.clone()).unwrap();

        let i_peer_set = store.get_peer_set(0).unwrap();
        assert_eq!(
            i_peer_set.peers, peer_set.peers,
            "InmemStore PeerSet mismatch"
        );

        let repertoire = store.db_get_repertoire().unwrap();
        for (pub_, peer) in &peer_set.by_pub_key {
            let p = repertoire.get(pub_).expect("Repertoire entry not found");
            assert_eq!(p, peer, "repertoire entry mismatch");
        }

        remove_store(store, &dir);
    }

    // Translation of badger_store_test.go::TestBadgerEvents.
    #[test]
    fn test_rocks_events() {
        let cache_size = 10i64;
        let test_size = 100i64;
        let (store, dir) = temp_store(cache_size);
        let (peer_set, participants) = init_peers(3);
        store.set_peer_set(0, Arc::new(peer_set)).unwrap();

        let mut events: HashMap<String, Vec<Event>> = HashMap::new();
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
                store.set_event(&event).unwrap();
                items.push(event);
            }
            events.insert(p.hex.clone(), items);
        }

        for (_p, evs) in &events {
            for ev in evs {
                let rev = store
                    .get_event(&ev.hex())
                    .or_else(|_| store.db_get_event(&ev.hex()))
                    .unwrap();
                assert_eq!(ev.body, rev.body, "event body mismatch");
                assert_eq!(ev.signature, rev.signature, "event signature mismatch");
            }
        }

        let skip_index = -1i64;
        for p in &participants {
            let p_events = store
                .participant_events(&p.hex, skip_index)
                .or_else(|_| store.db_participant_events(&p.hex, skip_index))
                .unwrap();
            assert_eq!(p_events.len() as i64, test_size, "participant event count");
            let expected = &events[&p.hex][(skip_index + 1) as usize..];
            for (k, e) in expected.iter().enumerate() {
                assert_eq!(e.hex(), p_events[k], "participant event mismatch");
            }
        }

        for p in &participants {
            let last = store.last_event_from(&p.hex).unwrap();
            let evs = &events[&p.hex];
            assert_eq!(last, evs[evs.len() - 1].hex(), "last event mismatch");
        }

        let known = store.known_events();
        for p in &participants {
            assert_eq!(known[&p.id], test_size - 1, "Incorrect Known");
        }

        for p in &participants {
            for ev in &events[&p.hex] {
                store.add_consensus_event(ev).unwrap();
            }
        }

        remove_store(store, &dir);
    }

    // Translation of badger_store_test.go::TestBadgerRounds.
    #[test]
    fn test_rocks_rounds() {
        let (store, dir) = temp_store(0);
        let (peer_set, participants) = init_peers(3);
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

        store.set_round(0, &round).unwrap();
        assert_eq!(store.last_round(), 0, "Store LastRound should be 0");

        let stored_round = store
            .get_round(0)
            .or_else(|_| store.db_get_round(0))
            .unwrap();
        assert_eq!(round, stored_round, "Round and StoredRound do not match");

        remove_store(store, &dir);
    }

    // Translation of badger_store_test.go::TestBadgerBlocks.
    #[test]
    fn test_rocks_blocks() {
        let (store, dir) = temp_store(0);
        let (peer_set, participants) = init_peers(3);
        store.set_peer_set(0, Arc::new(peer_set)).unwrap();

        let block = make_block(&participants);
        store.set_block(&block).unwrap();

        let stored_block = store.get_block(0).unwrap();
        assert_eq!(stored_block.body, block.body, "Block bodies mismatch");
        assert_eq!(
            stored_block.signatures, block.signatures,
            "Block signatures mismatch"
        );

        remove_store(store, &dir);
    }

    // Translation of badger_store_test.go::TestBadgerFrames.
    #[test]
    fn test_rocks_frames() {
        let (store, dir) = temp_store(0);
        let (peer_set, participants) = init_peers(3);
        store.set_peer_set(0, Arc::new(peer_set)).unwrap();

        let mut events: Vec<FrameEvent> = Vec::new();
        let mut roots: BTreeMap<String, Root> = BTreeMap::new();
        for p in &participants {
            let mut event = Event::new(
                vec![format!("{}_{}", &p.hex[..5], 0).into_bytes()],
                vec![],
                vec![BlockSignature {
                    validator: b"validator".to_vec(),
                    index: 0,
                    signature: "r|s".to_string(),
                }],
                vec![String::new(), String::new()],
                p.pub_key.clone(),
                0,
            );
            event.sign(&p.priv_key).unwrap();
            events.push(FrameEvent {
                core: Box::new(event),
                ..FrameEvent::default()
            });
            roots.insert(p.hex.clone(), Root::new());
        }

        let frame = Frame {
            round: 1,
            events,
            roots,
            ..Frame::default()
        };

        store.set_frame(&frame).unwrap();
        let stored_frame = store
            .get_frame(frame.round)
            .or_else(|_| store.db_get_frame(frame.round))
            .unwrap();
        assert_eq!(stored_frame, frame, "Frame and StoredFrame do not match");

        remove_store(store, &dir);
    }
}
