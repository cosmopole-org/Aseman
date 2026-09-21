//! Store messaging values shared by the store use cases and their adapters.
//!
//! Store and member identities are the legacy string identities (`{n}@{origin}`);
//! capsule adapters map them to canonical IDs, so the rules here stay identical for
//! every provider.

use serde::{Deserialize, Serialize};

/// One store, as legacy exposes it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoreRecord {
    pub id: String,
    /// Legacy `persHist`: every signal sent into the store is recorded.
    pub persistent_history: bool,
    pub signal_count: i64,
    pub tag: String,
    /// The parent store, or empty for a top-level store.
    pub parent_id: String,
    pub is_public: bool,
    /// Legacy stores at least 1.
    pub member_count: i64,
}

/// One recorded store signal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StoreSignal {
    pub id: String,
    pub store_id: String,
    pub sender_id: String,
    pub data: String,
    pub tags: Vec<String>,
    pub time_millis: i64,
    pub edited: bool,
}
