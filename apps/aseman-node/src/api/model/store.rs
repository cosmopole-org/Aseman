//! The legacy wire shape of a store (persisted as `core.store`, ADR 0036).

use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub tag: String,
    #[serde(rename = "parentId", default)]
    pub parent_id: String,
    #[serde(rename = "persHist", default)]
    pub pers_hist: bool,
    #[serde(rename = "isPublic", default)]
    pub is_public: bool,
    #[serde(rename = "memberCount", default)]
    pub member_count: i32,
    #[serde(rename = "signalCount", default)]
    pub signal_count: i64,
}
