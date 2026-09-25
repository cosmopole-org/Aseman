//! Translation of `chain/hashgraph` — the Babble distributed consensus
//! algorithm (Leemon Baird's Hashgraph).
//!
//! Phase 2 part 2 translates the data model: errors, events, blocks, frames,
//! roots, round info and internal transactions. The stores (`caches`,
//! `inmem_store`, the RocksDB store) and the consensus engine (`hashgraph.go`)
//! follow in subsequent parts.
//!
//! Serialization note: the Go code marshalled `Frame`/`RoundInfo` with
//! `ugorji/go/codec` in canonical mode (sorted keys). The translation uses
//! `serde_json` and `BTreeMap` for every map that participates in a hash, which
//! gives the same deterministic, order-independent output.

pub mod block;
pub mod caches;
pub mod errors;
pub mod event;
pub mod frame;
pub mod hashgraph;
pub mod inmem_store;
pub mod internal_transaction;
pub mod rocks_store;
pub mod root;
pub mod round_info;
pub mod store;

pub use block::{Block, BlockBody, BlockSignature, WireBlockSignature};
pub use caches::{
    Key, ParticipantEventsCache, PeerSetCache, PendingRound, PendingRoundsCache, SigPool, TreKey,
};
pub use errors::{SelfParentError, is_normal_self_parent_error};
pub use event::{
    CoordinatesMap, Event, EventBody, EventCoordinates, FrameEvent, WireBody, WireEvent,
    sort_by_topological_order,
};
pub use frame::Frame;
pub use hashgraph::{
    COIN_ROUND_FREQ, Hashgraph, InternalCommitCallback, ROOT_DEPTH, dummy_internal_commit_callback,
};
pub use inmem_store::InmemStore;
pub use internal_transaction::{
    InternalTransaction, InternalTransactionBody, InternalTransactionReceipt, TransactionType,
};
pub use rocks_store::RocksDbStore;
pub use root::Root;
pub use round_info::{RoundEvent, RoundInfo};
pub use store::Store;
