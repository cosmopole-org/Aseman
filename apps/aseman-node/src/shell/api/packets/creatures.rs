//! Request and response payloads for the `creatures` action namespace.
//!
//! Covers both the creature lifecycle (`Create`, `Signal`) and the
//! per-creature identity / token / authentication actions that used to
//! live under the `users` namespace.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::input::IInput;
use crate::shell::api::model::{Creature, Session};

macro_rules! input_impls {
    ($($t:ty => $origin:expr),* $(,)?) => {
        $(
            impl IInput for $t {
                fn get_store_id(&self) -> String { String::new() }
                fn origin(&self) -> String { $origin.to_string() }
                fn as_any(&self) -> &dyn std::any::Any { self }
            }
        )*
    };
}

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

impl IInput for CreateInput {
    fn get_store_id(&self) -> String {
        String::new()
    }
    fn origin(&self) -> String {
        "global".to_string()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
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

impl IInput for SignalInput {
    fn get_store_id(&self) -> String {
        self.store_id.clone()
    }
    fn origin(&self) -> String {
        String::new()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
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
    /// Optional step index inside a multi-step lock. Matches Go's `*int`
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
pub struct MintInput {
    #[serde(rename = "toUserEmail", default)]
    pub to_user_email: String,
    #[serde(default)]
    pub amount: i64,
    /// Caller's name for the payment being minted. Optional, but a caller that
    /// can be interrupted between a successful mint and recording it should
    /// always send one — it is what makes a retry safe. See the `mint` handler.
    #[serde(rename = "idempotencyKey", default)]
    pub idempotency_key: String,
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
pub struct PublishFinanceCatalogInput {
    #[serde(default)]
    pub catalog: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegisterFinanceNodeInput {
    #[serde(default)]
    pub node: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetireFinanceNodeInput {
    #[serde(rename = "nodeOwnerAccountId", default)]
    pub node_owner_account_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegisterFinanceResourceInput {
    #[serde(default)]
    pub resource: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewFinanceResourceInput {
    #[serde(rename = "resourceId", default)]
    pub resource_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetireFinanceResourceInput {
    #[serde(rename = "resourceId", default)]
    pub resource_id: String,
    #[serde(default)]
    pub kind: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PublishFinanceQuoteInput {
    #[serde(default)]
    pub quote: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateHoldInput {
    #[serde(rename = "quoteId", default)]
    pub quote_id: String,
    #[serde(rename = "pricingVersion", default)]
    pub pricing_version: String,
    #[serde(rename = "maxAmount", default)]
    pub max_amount: i64,
    #[serde(rename = "settlementAuthority", default)]
    pub settlement_authority: String,
    #[serde(rename = "meterProgramId", default)]
    pub meter_program_id: String,
    #[serde(rename = "expiresAt", default)]
    pub expires_at: i64,
    #[serde(rename = "idempotencyKey", default)]
    pub idempotency_key: String,
    #[serde(rename = "contextHash", default)]
    pub context_hash: String,
    #[serde(rename = "beneficiaryPlanHash", default)]
    pub beneficiary_plan_hash: String,
    #[serde(default)]
    pub beneficiaries: Vec<HoldBeneficiaryInput>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HoldBeneficiaryInput {
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub role: String,
    #[serde(rename = "maxAmount", default)]
    pub max_amount: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StartHoldInput {
    #[serde(rename = "holdId", default)]
    pub hold_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "quoteId", default)]
    pub quote_id: String,
    #[serde(rename = "runId", default)]
    pub run_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettleHoldInput {
    #[serde(rename = "holdId", default)]
    pub hold_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "quoteId", default)]
    pub quote_id: String,
    #[serde(rename = "settlementId", default)]
    pub settlement_id: String,
    #[serde(rename = "usageHash", default)]
    pub usage_hash: String,
    #[serde(default)]
    pub lines: Vec<SettlementLineInput>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettlementLineInput {
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub amount: i64,
    #[serde(rename = "sourceRef", default)]
    pub source_ref: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseHoldInput {
    #[serde(rename = "holdId", default)]
    pub hold_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "releaseId", default)]
    pub release_id: String,
    #[serde(default)]
    pub reason: String,
}

// ── Shared authorization pool (replaces per-run holds) ───────────────────────
// A pool is a per-user standing reservation that many runs draw down together;
// see docs/SHARED-POOL-DESIGN.md in decillionai-server. openPool/refreshPool/
// closePool are client-signed (payer); reservePool/settlePool/releasePool are
// meter-signed (settlement authority).

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenPoolInput {
    #[serde(rename = "maxAmount", default)]
    pub max_amount: i64,
    #[serde(rename = "settlementAuthority", default)]
    pub settlement_authority: String,
    #[serde(rename = "meterProgramId", default)]
    pub meter_program_id: String,
    #[serde(rename = "expiresAt", default)]
    pub expires_at: i64,
    #[serde(rename = "idempotencyKey", default)]
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RefreshPoolInput {
    #[serde(rename = "poolId", default)]
    pub pool_id: String,
    #[serde(rename = "refreshId", default)]
    pub refresh_id: String,
    #[serde(default)]
    pub amount: i64,
    #[serde(rename = "expiresAt", default)]
    pub expires_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClosePoolInput {
    #[serde(rename = "poolId", default)]
    pub pool_id: String,
    #[serde(rename = "closeId", default)]
    pub close_id: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReservePoolInput {
    #[serde(rename = "poolId", default)]
    pub pool_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "quoteId", default)]
    pub quote_id: String,
    #[serde(rename = "runId", default)]
    pub run_id: String,
    #[serde(rename = "maxAmount", default)]
    pub max_amount: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettlePoolInput {
    #[serde(rename = "poolId", default)]
    pub pool_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "quoteId", default)]
    pub quote_id: String,
    #[serde(rename = "runId", default)]
    pub run_id: String,
    #[serde(rename = "settlementId", default)]
    pub settlement_id: String,
    #[serde(rename = "usageHash", default)]
    pub usage_hash: String,
    #[serde(default)]
    pub lines: Vec<SettlementLineInput>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleasePoolInput {
    #[serde(rename = "poolId", default)]
    pub pool_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "runId", default)]
    pub run_id: String,
    #[serde(rename = "releaseId", default)]
    pub release_id: String,
    #[serde(default)]
    pub reason: String,
}

// Live incremental debit against the shared pool. Unlike reservePool/settlePool
// (which reserve a per-run slice up front and settle actual ≤ slice), debitPool
// spends directly from the pool's shared `remaining` as a run accrues cost —
// with NO per-run ceiling. The pool's `remaining` is the single live counter all
// of a payer's concurrent runs (across every space) draw down together; when a
// debit would exceed it, the op returns `{applied:false, exhausted:true}` so the
// meter can stop the run peacefully. Meter-signed (settlement authority).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DebitPoolInput {
    #[serde(rename = "poolId", default)]
    pub pool_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
    #[serde(rename = "quoteId", default)]
    pub quote_id: String,
    #[serde(rename = "runId", default)]
    pub run_id: String,
    #[serde(rename = "debitId", default)]
    pub debit_id: String,
    #[serde(rename = "usageHash", default)]
    pub usage_hash: String,
    #[serde(default)]
    pub lines: Vec<SettlementLineInput>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetHoldInput {
    #[serde(rename = "holdId", default)]
    pub hold_id: String,
    #[serde(rename = "payerUserId", default)]
    pub payer_user_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetFinancialAccountInput {
    /// Defaults to the authenticated caller. Only the network owner may inspect
    /// another creature's private financial account.
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub limit: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconcileFinancialSystemInput {
    #[serde(rename = "maxIssues", default)]
    pub max_issues: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestPayoutInput {
    #[serde(rename = "requestId", default)]
    pub request_id: String,
    #[serde(default)]
    pub amount: i64,
    /// Opaque token issued by the configured payout processor. Never send bank
    /// or card details to the chain.
    #[serde(rename = "destinationRef", default)]
    pub destination_ref: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolvePayoutInput {
    #[serde(rename = "payoutId", default)]
    pub payout_id: String,
    #[serde(rename = "resolutionId", default)]
    pub resolution_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(rename = "providerReference", default)]
    pub provider_reference: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListPayoutsInput {
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub limit: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaymentAdjustmentInput {
    /// Target creature id. Payment processors resolve and authenticate this
    /// server-side; the browser never submits this action directly.
    #[serde(rename = "userId", default)]
    pub user_id: String,
    /// Positive credits the available wallet; negative reverses a payment. A
    /// reversal larger than the available wallet becomes recoverable debt.
    #[serde(default)]
    pub amount: i64,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub reference: String,
    #[serde(rename = "idempotencyKey", default)]
    pub idempotency_key: String,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferInput {
    #[serde(rename = "toUsername", default)]
    pub to_username: String,
    #[serde(default)]
    pub amount: i64,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoginInput {
    #[serde(default)]
    pub username: String,
    #[serde(rename = "emailToken", default)]
    pub email_token: String,
    #[serde(default)]
    pub metadata: Value,
    /// A single-use grant from `grantLogin`, required when the node runs with
    /// `CASPAR_LOGIN_MODE=grant`.
    #[serde(rename = "loginGrant", default)]
    pub login_grant: String,
}

input_impls! {
    ConsumeLockInput => "global",
    GetInput         => "",
    FindInput        => "",
    ListInput        => "",
    MetaInput        => "global",
    DeleteInput      => "global",
    CheckSignInput   => "",
    AuthenticateInput => "",
    GetByUsernameInput => "",
    MintInput        => "global",
    LockTokenInput   => "global",
    PublishFinanceCatalogInput => "global",
    RegisterFinanceNodeInput => "global",
    RetireFinanceNodeInput => "global",
    RegisterFinanceResourceInput => "global",
    ReviewFinanceResourceInput => "global",
    RetireFinanceResourceInput => "global",
    PublishFinanceQuoteInput => "global",
    CreateHoldInput  => "global",
    StartHoldInput   => "global",
    SettleHoldInput  => "global",
    ReleaseHoldInput => "global",
    OpenPoolInput    => "global",
    RefreshPoolInput => "global",
    ClosePoolInput   => "global",
    ReservePoolInput => "global",
    SettlePoolInput  => "global",
    ReleasePoolInput => "global",
    DebitPoolInput   => "global",
    GetHoldInput     => "global",
    GetFinancialAccountInput => "global",
    ReconcileFinancialSystemInput => "global",
    RequestPayoutInput => "global",
    ResolvePayoutInput => "global",
    ListPayoutsInput => "global",
    PaymentAdjustmentInput => "global",
    TransferInput    => "global",
    UpdateInput      => "global",
    LoginInput       => "",
    SecretPutInput    => "global",
    SecretGetInput    => "global",
    SecretGrantInput  => "global",
    SecretRevokeInput => "global",
    SecretListInput   => "global",
    SecretListGrantedInput => "global",
    StorageUploadInput => "global",
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoginOutput {
    #[serde(default)]
    pub user: Creature,
    #[serde(default)]
    pub session: Session,
    #[serde(rename = "privateKey", default)]
    pub private_key: String,
}
