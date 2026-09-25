//! Chain consensus, election and staking data types — everything carried
//! on the wire between chain peers.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::update::Update;
use crate::util::GoError;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ChainMessage {
    pub key: String,
    pub message_type: String,
    pub author: String,
    pub submitter: String,
    #[serde(with = "crate::util::bytes_base64")]
    pub payload: Vec<u8>,
    pub signatures: Vec<String>,
    pub request_id: String,
    pub recievers: HashMap<String, HashMap<String, bool>>,
    pub reply_to: String,
    pub store_id: String,
    pub pay: Option<ChainPayPacket>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ChainPayPacket {
    #[serde(rename = "Type")]
    pub typ: String,
    pub session_id: String,
    pub machine_ids: Vec<String>,
    pub user_id: String,
    pub lock_id: String,
    pub lock_signature: String,
    pub amount: i64,
    pub requested_seconds: i64,
    pub accepted_seconds: i64,
    pub cost_per_second: i64,
    pub error: String,
    pub store_id: String,
    pub vm_payload: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ChainBaseRequest {
    pub key: String,
    pub author: String,
    pub submitter: String,
    #[serde(with = "crate::util::bytes_base64")]
    pub payload: Vec<u8>,
    pub signatures: Vec<String>,
    pub request_id: String,
    pub tag: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ChainResponse {
    pub executor: String,
    #[serde(with = "crate::util::bytes_base64")]
    pub payload: Vec<u8>,
    pub signature: String,
    pub request_id: String,
    pub effects: Effects,
    pub res_code: i64,
    pub err: String,
    pub tag: String,
    pub to_user_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ChainAppletRequest {
    pub machine_id: String,
    pub key: String,
    pub author: String,
    pub submitter: String,
    #[serde(with = "crate::util::bytes_base64")]
    pub payload: Vec<u8>,
    pub signatures: Vec<String>,
    pub request_id: String,
    pub runtime: String,
    pub tag: String,
    pub token_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ChainElectionPacket {
    #[serde(rename = "Type")]
    pub typ: String,
    pub key: String,
    pub meta: HashMap<String, Value>,
    #[serde(with = "crate::util::bytes_base64")]
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChainStakePacket {
    #[serde(rename = "nodeId")]
    pub node_id: String,
    #[serde(rename = "ownerId")]
    pub owner_id: String,
    #[serde(rename = "action")]
    pub action: String,
    #[serde(rename = "amount")]
    pub amount: i64,
    #[serde(rename = "nonce")]
    pub nonce: u64,
    #[serde(rename = "lockSeconds")]
    pub lock_seconds: i64,
    #[serde(rename = "reason")]
    pub reason: String,
    #[serde(rename = "timestamp")]
    pub timestamp: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StakingNodeState {
    #[serde(rename = "nodeId")]
    pub node_id: String,
    #[serde(rename = "ownerId")]
    pub owner_id: String,
    #[serde(rename = "bondedStake")]
    pub bonded_stake: i64,
    #[serde(rename = "pendingUnbond")]
    pub pending_unbond: i64,
    #[serde(rename = "unbondUnlockAt")]
    pub unbond_unlock_at: i64,
    #[serde(rename = "nonce")]
    pub nonce: u64,
    #[serde(rename = "lastUpdatedAt")]
    pub last_updated_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ValidatorSetRecord {
    #[serde(rename = "roundId")]
    pub round_id: String,
    #[serde(rename = "validators")]
    pub validators: Vec<String>,
    #[serde(rename = "totalBonded")]
    pub total_bonded: i64,
    #[serde(rename = "selectedAt")]
    pub selected_at: i64,
    #[serde(rename = "eligibleNode")]
    pub eligible_node: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ElectionRound {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "commitSeed")]
    pub commit_seed: String,
    #[serde(rename = "phase")]
    pub phase: String,
    #[serde(rename = "startAt")]
    pub start_at: i64,
    #[serde(rename = "commitDeadline")]
    pub commit_deadline: i64,
    #[serde(rename = "revealDeadline")]
    pub reveal_deadline: i64,
    #[serde(rename = "finalizedAt")]
    pub finalized_at: i64,
    #[serde(rename = "commits")]
    pub commits: HashMap<String, String>,
    #[serde(rename = "reveals")]
    pub reveals: HashMap<String, String>,
    #[serde(rename = "commitParticipants")]
    pub commit_participants: HashMap<String, bool>,
    #[serde(rename = "selectedValidators")]
    pub selected_validators: Vec<String>,
}

/// In-flight election bookkeeping. Held in memory only; not serialized.
#[derive(Debug, Clone, Default)]
pub struct Election {
    pub my_num: String,
    pub participants: HashMap<String, bool>,
    pub commits: HashMap<String, Vec<u8>>,
    pub reveals: HashMap<String, String>,
}

/// Callback awaiting responses from a set of chain executors.
#[derive(Clone)]
pub struct ChainCallback {
    pub fn_: Arc<dyn Fn(Vec<u8>, i64, Option<GoError>) + Send + Sync>,
    pub executors: HashMap<String, bool>,
    pub responses: HashMap<String, String>,
    pub tag: String,
}

/// Callback awaiting a single typed chain message reply.
#[derive(Clone)]
pub struct MessageCallback {
    pub id: String,
    pub fn_: Arc<dyn Fn(String, Vec<u8>) + Send + Sync>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Effects {
    #[serde(rename = "dbUpdates", default)]
    pub db_updates: Vec<Update>,
}
