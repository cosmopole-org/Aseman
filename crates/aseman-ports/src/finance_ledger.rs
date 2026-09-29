//! The finance ledger port.
//!
//! The finance family is a set of state machines over JSON documents
//! (`Json::FinanceHold::*`, `Json::FinancePool::*`, `Json::FinancePayout::*`,
//! `Json::BillingCatalog::*`, `Json::BillingQuote::*`, `Json::CreatureNamespace::*`)
//! and integer link counters (`FinanceHeld::`, `FinanceDebt::`, `FinanceWithdrawable::`,
//! `FinancePayoutHeld::`, `FinanceEarned::`, `FinanceSpent::`). The ledger port exposes
//! those operations without naming the concrete store; the node implements it over its
//! storage transaction, on any storage provider.
//!
//! Document semantics: for the id-scoped families the `id` names the record and the
//! `path` is the fixed document label (`hold`, `pool`, `payout`, ...). `BillingCurrent`
//! and `BillingNodes` are the shared `Json::CreatureNamespace::billing` document at the
//! `current` and `nodes` paths; `Market` is the shared `Json::CreatureNamespace::market`
//! document at the bucket path, so the bucket is passed as `id`.

use serde_json::{Map, Value};

use crate::{PortError, PortResult};

/// A finance JSON document family. The id distinguishes the record; `BillingNodes`,
/// `BillingCurrent`, and `Market` are namespace documents whose "id" is the bucket.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FinanceDoc {
    Hold,
    Pool,
    PoolReservation,
    LiveDebit,
    ProjectBudget,
    Journal,
    Payout,
    BillingCatalog,
    BillingQuote,
    BillingCurrent,
    BillingNodes,
    Market,
    /// The `Json::Creature::{id}` document, whose `lockedTokens.{lock_id}` path the
    /// token-lock family reads and writes.
    Creature,
}

/// A finance wallet counter family, stored as an integer link per user.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum WalletCounter {
    Held,
    Debt,
    Withdrawable,
    PayoutHeld,
    Earned,
    Spent,
}

/// An idempotency marker link. Each variant names the idempotency-key family and the
/// identifiers that make one key unique, exactly as the finance records are keyed
/// them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FinanceMarker {
    /// `FinanceHoldRequest::{payer}::{key}` → `"{hold_id}|{request_hash}"`.
    HoldRequest { payer: String, key: String },
    /// `FinanceRun::{authority}::{run_id}` → hold id.
    Run { authority: String, run_id: String },
    /// `FinanceSettlement::{authority}::{settlement_id}` → hold id.
    Settlement {
        authority: String,
        settlement_id: String,
    },
    /// `FinanceRelease::{caller}::{release_id}` → hold id.
    Release { caller: String, release_id: String },
    /// `FinancePayoutRequest::{user}::{request_id}` → `"{payout_id}|{request_hash}"`.
    PayoutRequest { user: String, request_id: String },
    /// `FinancePayoutResolution::{resolution_id}` → `"{payout_id}|{request_hash}"`.
    PayoutResolution { resolution_id: String },
    /// `FinancePoolOpen::{payer}::{idempotency_key}` → pool id.
    PoolOpen { payer: String, key: String },
    /// `FinancePoolRefresh::{payer}::{refresh_id}` → pool id.
    PoolRefresh { payer: String, refresh_id: String },
    /// `FinancePoolClose::{pool_id}` → close id.
    PoolClose { pool_id: String },
    /// `FinancePoolSettlement::{authority}::{settlement_id}` → run id.
    PoolSettlement {
        authority: String,
        settlement_id: String,
    },
    /// `FinancePoolDebit::{authority}::{debit_id}` → run id.
    PoolDebit { authority: String, debit_id: String },
    /// `PaymentAdjustment::{idempotency_key}` → `"{request_hash}|{journal_id}"`.
    PaymentAdjustment { key: String },
    /// `MintApplied::{idempotency_key}` → `"{target}:{amount}:{journal_id}"`.
    MintApplied { key: String },
}

/// The finance ledger behind the finance use cases.
pub trait FinanceLedger: Send + Sync {
    // ---- JSON documents ----

    /// Read a finance document. `NotFound` when the document (or its path) is absent.
    fn get_doc(&self, family: FinanceDoc, id: &str, path: &str) -> PortResult<Map<String, Value>>;

    /// Write a finance document. `merge` deep-merges into the existing document, as
    /// `put_json(.., merge)` did.
    fn put_doc(
        &self,
        family: FinanceDoc,
        id: &str,
        path: &str,
        value: &Value,
        merge: bool,
    ) -> PortResult<()>;

    /// The ids of every document of `family`, in the store's scan order.
    fn doc_ids(&self, family: FinanceDoc) -> PortResult<Vec<String>>;

    // ---- wallet counters ----

    /// Read a wallet counter (zero when no link exists).
    fn counter(&self, kind: WalletCounter, user: &str) -> PortResult<i64>;

    /// Set a wallet counter.
    fn set_counter(&self, kind: WalletCounter, user: &str, amount: i64) -> PortResult<()>;

    /// Add `amount` (>= 0) to a wallet counter and return the new value. `Denied` on
    /// an overflow or a negative amount.
    fn add_counter(&self, kind: WalletCounter, user: &str, amount: i64) -> PortResult<i64>;

    /// Every (user, raw stored value) pair of a wallet counter, for reconciliation.
    /// The raw value lets the caller validate it (a non-numeric or negative stored
    /// counter is an issue, not a silent zero).
    fn counter_links(&self, kind: WalletCounter) -> PortResult<Vec<(String, String)>>;

    // ---- idempotency markers and index links ----

    /// Read a marker link (empty string when absent).
    fn marker(&self, marker: &FinanceMarker) -> PortResult<String>;

    /// Write a marker link.
    fn put_marker(&self, marker: &FinanceMarker, value: &str) -> PortResult<()>;

    /// The hold ids indexed under a payer, newest first (`FinanceHoldByPayer::{user}::`).
    fn hold_ids_by_payer(&self, user: &str, limit: usize) -> PortResult<Vec<String>>;

    /// The journal ids indexed under a user, newest first (`FinanceJournalByUser::{user}::`).
    fn journal_ids_by_user(&self, user: &str, limit: usize) -> PortResult<Vec<String>>;

    /// The payout ids indexed under a user, newest first (`FinancePayoutByUser::{user}::`).
    fn payout_ids_by_user(&self, user: &str, limit: usize) -> PortResult<Vec<String>>;

    /// The user's current open pool id (`FinancePoolByUser::{user}`), empty when none.
    fn pool_of_user(&self, user: &str) -> PortResult<String>;

    /// Bind a pool to its payer (`FinancePoolByUser::{user}`).
    fn put_pool_of_user(&self, user: &str, pool_id: &str) -> PortResult<()>;

    // ---- email identity links (mint and login) ----

    /// The creature id bound to an email (`UserEmailToId::{email}`), empty when none.
    fn email_to_id(&self, email: &str) -> PortResult<String>;

    /// Bind an email to a creature id.
    fn put_email_to_id(&self, email: &str, user_id: &str) -> PortResult<()>;

    /// The email bound to a creature id (`UserIdToEmail::{user_id}`), empty when none.
    fn id_to_email(&self, user_id: &str) -> PortResult<String>;

    /// Bind a creature id to an email.
    fn put_id_to_email(&self, user_id: &str, email: &str) -> PortResult<()>;

    // ---- journal ----

    /// Append a finance journal entry and index it under every participant, returning
    /// the new journal id. `participants` are deduplicated and empty ids skipped.
    fn write_journal(
        &self,
        kind: &str,
        hold_id: &str,
        payer_id: &str,
        payload: Value,
        participants: &[String],
        now: i64,
    ) -> PortResult<String>;

    // ---- id generation ----

    /// A fresh identifier for a hold, pool, payout, or journal.
    fn gen_id(&self) -> String;
}

/// The fixed document label of a family, for error messages and adapter key building.
#[must_use]
pub fn doc_path(family: FinanceDoc) -> &'static str {
    match family {
        FinanceDoc::Hold => "hold",
        FinanceDoc::Pool => "pool",
        FinanceDoc::PoolReservation => "reservation",
        FinanceDoc::LiveDebit => "debit",
        FinanceDoc::ProjectBudget => "budget",
        FinanceDoc::Journal => "entry",
        FinanceDoc::Payout => "payout",
        FinanceDoc::BillingCatalog => "catalog",
        FinanceDoc::BillingQuote => "quote",
        FinanceDoc::BillingCurrent => "current",
        FinanceDoc::BillingNodes => "nodes",
        FinanceDoc::Market => "market",
        FinanceDoc::Creature => "lockedTokens",
    }
}

/// The `PortError::NotFound` an absent document reads as.
#[must_use]
pub fn absent() -> PortError {
    PortError::NotFound
}
