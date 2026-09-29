//! Wire shapes of the creature, identity, secret, lock, and file operations. The
//! finance operations take the finance use cases' own inputs.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

fn i64_is_zero(n: &i64) -> bool {
    *n == 0
}

// ---- Inputs ----------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateInput {
    #[serde(rename = "type", default)]
    pub typ: String,
    #[serde(default)]
    pub username: String,
    #[serde(rename = "publicKey", default)]
    pub public_key: String,
    #[serde(rename = "chainId", default, skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<String>,
    #[serde(
        rename = "subchainId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub subchain_id: Option<String>,
    #[serde(rename = "ownerId", default, skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SignalInput {
    #[serde(rename = "type", default)]
    pub typ: String,
    #[serde(default)]
    pub data: String,
    #[serde(rename = "storeId", default)]
    pub store_id: String,
    #[serde(rename = "creatureId", default)]
    pub creature_id: String,
    #[serde(
        rename = "programId",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub program_id: String,
    #[serde(rename = "entityId", default, skip_serializing_if = "String::is_empty")]
    pub entity_id: String,
    /// Optional correlation id stamped on the outgoing signal packet. When
    /// the target entity is a proxy entity, the same id is used to route the
    /// eventual response signal back through the proxy to this sender.
    #[serde(
        rename = "correlationId",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub correlation_id: String,
    #[serde(default)]
    pub temp: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsumeLockInput {
    #[serde(rename = "type", default)]
    pub typ: String,
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(rename = "lockId", default)]
    pub lock_id: String,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub amount: i64,
    /// Optional step index inside a multi-step lock. Absent
    /// (`nil` = auto-pick the next consumable step).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetInput {
    #[serde(rename = "userId", default)]
    pub user_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FindInput {
    #[serde(default)]
    pub username: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListInput {
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub count: i64,
    #[serde(default)]
    pub param: String,
    #[serde(default)]
    pub query: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetaInput {
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeleteInput {
    #[serde(rename = "userId", default)]
    pub user_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheckSignInput {
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub payload: String,
    #[serde(default)]
    pub signature: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthenticateInput {}

// ---- creature-owned secrets ------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretPutInput {
    /// The secret's name within the owner's namespace (e.g. "LLM_KEY_OPENAI").
    #[serde(default)]
    pub name: String,
    /// The plaintext to encrypt and store. Never persisted in the clear.
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretGetInput {
    #[serde(default)]
    pub name: String,
    /// Whose secret to read. Defaults to the caller; a different owner requires
    /// an unexpired grant to the caller.
    #[serde(default)]
    pub owner: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretGrantInput {
    #[serde(default)]
    pub name: String,
    /// The creature id being granted temporary read access.
    #[serde(default)]
    pub grantee: String,
    /// How long the grant is valid, in seconds. Must be positive; the grant is
    /// revocable before then via `secretRevoke`.
    #[serde(rename = "ttlSeconds", default)]
    pub ttl_seconds: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretRevokeInput {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub grantee: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretListInput {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretListGrantedInput {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StorageUploadInput {
    /// The file bytes, base64-encoded. Content lives off-chain in the node's
    /// public-files storage; only the returned id is meant to go on-chain.
    #[serde(rename = "dataBase64", default)]
    pub data_base64: String,
    #[serde(rename = "contentType", default)]
    pub content_type: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetByUsernameInput {
    #[serde(default)]
    pub username: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LockTokenInput {
    #[serde(default, skip_serializing_if = "i64_is_zero")]
    pub amount: i64,
    #[serde(rename = "type", default)]
    pub typ: String,
    #[serde(default)]
    pub target: String,
    #[serde(rename = "unlockAt", default, skip_serializing_if = "i64_is_zero")]
    pub unlock_at: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<LockTokenStepInput>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LockTokenStepInput {
    #[serde(default)]
    pub amount: i64,
    #[serde(rename = "unlockAt", default)]
    pub unlock_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateInput {
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub metadata: Value,
    #[serde(rename = "publicKey", default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub typ: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

// ---- Outputs ---------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthenticateOutput {
    #[serde(default)]
    pub authenticated: bool,
    #[serde(default)]
    pub user: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetOutput {
    #[serde(default)]
    pub user: HashMap<String, Value>,
}
