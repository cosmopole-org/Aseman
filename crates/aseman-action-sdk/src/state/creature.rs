//! The wire shape of a creature (persisted as `core.creature` and
//! `core.user`, with its balance in `finance.wallet`, ADR 0036).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Creature {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default)]
    pub type_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(
        rename = "publicKey",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub public_key: String,
    #[serde(rename = "chainId", default)]
    pub chain_id: String,
    #[serde(rename = "subchainId", default)]
    pub subchain_id: String,
    #[serde(rename = "ownerId", default, skip_serializing_if = "String::is_empty")]
    pub owner_id: String,
    #[serde(rename = "machinesCount", default, skip_serializing_if = "i64_is_zero")]
    pub machines_count: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub avatar: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub desc: String,
    #[serde(default)]
    pub balance: i64,
}

fn i64_is_zero(n: &i64) -> bool {
    *n == 0
}