//! Metering, pricing, and the ledger (Phase 8).
//!
//! Money is integer minor units. Never a float: a binary float cannot represent a
//! tenth, and a charge that is off by a ten-thousandth of a unit every minute is a
//! charge nobody can reconcile.
//!
//! The one identity the whole phase turns on:
//!
//! ```text
//! (workload_id, interval_start, provider_sample_id)
//! ```
//!
//! A settlement is that triple. Collecting the same sample twice, or delivering the
//! same message twice, therefore cannot charge twice — not because the pipeline is
//! careful, but because the second attempt is the same identity.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Uuid;

/// Money, in the smallest unit the currency has. Signed, because a journal has both
/// sides and a refund is a real thing.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Minor(pub i64);

impl Minor {
    /// # Errors
    ///
    /// [`FinanceError::Overflow`] rather than a wrapped total.
    pub fn checked_add(self, other: Self) -> Result<Self, FinanceError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(FinanceError::Overflow)
    }

    /// # Errors
    ///
    /// [`FinanceError::Overflow`].
    pub fn checked_sub(self, other: Self) -> Result<Self, FinanceError> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(FinanceError::Overflow)
    }
}

/// What a runtime can be metered on.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    /// Milliseconds of CPU.
    CpuMillis,
    /// Mebibyte-seconds of memory.
    MemoryMibSeconds,
    /// Mebibyte-seconds of persistent storage.
    StorageMibSeconds,
    /// Bytes read from disk.
    DiskReadBytes,
    /// Bytes written to disk.
    DiskWriteBytes,
    /// Bytes in. Never billable: a workload does not choose what is sent to it.
    NetworkIngressBytes,
    /// Bytes out.
    NetworkEgressBytes,
    /// Milliseconds of an accelerator.
    AcceleratorMillis,
}

impl Dimension {
    /// Whether a price may be attached to this dimension.
    ///
    /// Ingress is deliberately unbillable: a workload cannot refuse what is sent to
    /// it, so charging for it would let anyone on the internet spend a creature's
    /// balance.
    #[must_use]
    pub const fn billable(self) -> bool {
        !matches!(self, Self::NetworkIngressBytes)
    }
}

/// One provider reading, as collected.
///
/// Provider counters are cumulative; the meter turns consecutive readings into the
/// interval deltas that are priced.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSample {
    pub workload_id: Uuid,
    /// The provider's own identity for this reading. Part of the settlement identity,
    /// so the same reading collected twice settles once.
    pub provider_sample_id: String,
    /// Which provider produced it, for reconciliation.
    pub provider: String,
    pub collected_at_millis: i64,
    /// Cumulative counters since the workload started.
    pub cumulative: BTreeMap<Dimension, u64>,
}

/// What a workload consumed during one interval.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageInterval {
    pub workload_id: Uuid,
    pub interval_start_millis: i64,
    pub interval_end_millis: i64,
    pub provider_sample_id: String,
    pub deltas: BTreeMap<Dimension, u64>,
}

impl UsageInterval {
    /// The settlement identity: one interval settles once, whatever happens upstream.
    #[must_use]
    pub fn settlement_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.workload_id, self.interval_start_millis, self.provider_sample_id
        )
    }
}

/// Turn two consecutive cumulative readings into the interval between them.
///
/// # Errors
///
/// [`FinanceError::OutOfOrder`] when the readings are not consecutive in time, and
/// [`FinanceError::DifferentWorkloads`] when they are not the same workload's.
pub fn interval_between(
    previous: &UsageSample,
    current: &UsageSample,
) -> Result<UsageInterval, FinanceError> {
    if previous.workload_id != current.workload_id {
        return Err(FinanceError::DifferentWorkloads);
    }
    if current.collected_at_millis <= previous.collected_at_millis {
        return Err(FinanceError::OutOfOrder);
    }
    let mut deltas = BTreeMap::new();
    for (dimension, now) in &current.cumulative {
        let before = previous.cumulative.get(dimension).copied().unwrap_or(0);
        // A counter that went backwards means the workload restarted and its counters
        // reset. The honest reading is the new counter itself, not a huge negative
        // treated as unsigned — which would be an enormous charge.
        let delta = now.checked_sub(before).unwrap_or(*now);
        deltas.insert(*dimension, delta);
    }
    Ok(UsageInterval {
        workload_id: current.workload_id,
        interval_start_millis: previous.collected_at_millis,
        interval_end_millis: current.collected_at_millis,
        provider_sample_id: current.provider_sample_id.clone(),
        deltas,
    })
}

/// A versioned price list. A charge names the version that produced it, so every
/// charge can be recomputed years later.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceList {
    pub version: String,
    /// Effective from this instant, inclusive.
    pub effective_from_millis: i64,
    /// Minor units per unit of each dimension, scaled by [`PRICE_SCALE`] so that a
    /// price below one minor unit per byte is still exact.
    pub rates: BTreeMap<Dimension, u64>,
}

impl PriceList {
    /// Whether every rate it names is expressible: a rate of zero for a dimension
    /// someone meant to charge for is a silent free ride, and is almost always a
    /// scaling mistake rather than a deliberate price.
    ///
    /// # Errors
    ///
    /// [`FinanceError::ZeroRate`] naming the dimension.
    pub fn validate(&self) -> Result<(), FinanceError> {
        for (dimension, rate) in &self.rates {
            if *rate == 0 {
                return Err(FinanceError::ZeroRate(*dimension));
            }
            if !dimension.billable() {
                return Err(FinanceError::UnbillableDimension(*dimension));
            }
        }
        Ok(())
    }
}

/// Rates are per unit of a dimension, scaled: a rate of 1 costs one minor unit per
/// [`PRICE_SCALE`] units. Integer arithmetic throughout, so a charge is exact and
/// reproducible on any node at any time.
///
/// A billion, not a million: per-byte prices are small. At a million, "one minor unit
/// per mebibyte" rounds to a rate of zero and the dimension silently becomes free.
pub const PRICE_SCALE: u64 = 1_000_000_000;

/// What an interval costs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Charge {
    pub settlement_key: String,
    pub workload_id: Uuid,
    pub amount: Minor,
    /// The price list that produced it.
    pub price_version: String,
    /// What each dimension contributed, so a charge can be explained line by line.
    pub lines: BTreeMap<Dimension, Minor>,
}

/// Price an interval. Deterministic: the same interval and price list always produce
/// the same charge, on any node, at any time.
///
/// # Errors
///
/// [`FinanceError::UnpricedDimension`] when the interval uses a billable dimension the
/// price list does not name — silently charging zero for something a price list forgot
/// would be a revenue hole nobody notices.
pub fn price(interval: &UsageInterval, prices: &PriceList) -> Result<Charge, FinanceError> {
    let mut lines = BTreeMap::new();
    let mut total = Minor(0);
    for (dimension, amount) in &interval.deltas {
        if !dimension.billable() {
            continue;
        }
        if *amount == 0 {
            continue;
        }
        let rate = prices
            .rates
            .get(dimension)
            .copied()
            .ok_or(FinanceError::UnpricedDimension(*dimension))?;
        // Rounded up: a fraction of a minor unit consumed is a minor unit owed, and
        // rounding down would make a busy workload free in the small.
        let cost = amount
            .checked_mul(rate)
            .map(|product| product.div_ceil(PRICE_SCALE))
            .ok_or(FinanceError::Overflow)?;
        let cost = Minor(i64::try_from(cost).map_err(|_| FinanceError::Overflow)?);
        if cost.0 > 0 {
            lines.insert(*dimension, cost);
            total = total.checked_add(cost)?;
        }
    }
    Ok(Charge {
        settlement_key: interval.settlement_key(),
        workload_id: interval.workload_id,
        amount: total,
        price_version: prices.version.clone(),
        lines,
    })
}

/// The price list in force for an instant: the newest one that had taken effect.
#[must_use]
pub fn price_list_at(lists: &[PriceList], at_millis: i64) -> Option<&PriceList> {
    lists
        .iter()
        .filter(|list| list.effective_from_millis <= at_millis)
        .max_by_key(|list| list.effective_from_millis)
}

/// One side of a double-entry record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub account: String,
    pub amount: Minor,
}

/// An append-only journal record. Debits and credits balance, or it is not a record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalRecord {
    /// The idempotency key of the mutation that produced it. For a settlement, the
    /// settlement key.
    pub idempotency_key: String,
    pub at_millis: i64,
    pub entries: Vec<Entry>,
    /// The price version, when this record came from a charge, so every entry traces
    /// back to a sample and a price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_version: Option<String>,
}

impl JournalRecord {
    /// Whether the record balances.
    #[must_use]
    pub fn balances(&self) -> bool {
        self.entries
            .iter()
            .try_fold(0_i64, |total, entry| total.checked_add(entry.amount.0))
            == Some(0)
    }
}

/// The double-entry record that settles a charge against a creature's wallet.
///
/// # Errors
///
/// [`FinanceError::Unbalanced`] can never escape this function — it is the assertion
/// that the construction is right. [`FinanceError::Overflow`] on an impossible amount.
pub fn settle(
    charge: &Charge,
    wallet: &str,
    revenue: &str,
    at_millis: i64,
) -> Result<JournalRecord, FinanceError> {
    let record = JournalRecord {
        idempotency_key: charge.settlement_key.clone(),
        at_millis,
        entries: vec![
            Entry {
                account: wallet.to_owned(),
                amount: Minor(
                    charge
                        .amount
                        .0
                        .checked_neg()
                        .ok_or(FinanceError::Overflow)?,
                ),
            },
            Entry {
                account: revenue.to_owned(),
                amount: charge.amount,
            },
        ],
        price_version: Some(charge.price_version.clone()),
    };
    if !record.balances() {
        return Err(FinanceError::Unbalanced);
    }
    Ok(record)
}

/// A hold placed on a balance before work is done.
///
/// Reserving is how a creature is stopped from spending the same balance twice over
/// while several workloads run: the money is committed before it is earned. A hold is
/// captured for what was actually used, or released.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hold {
    /// The idempotency key of the reservation, so re-reserving is not double-holding.
    pub idempotency_key: String,
    pub account: String,
    pub amount: Minor,
    pub state: HoldState,
}

/// Where a hold is in its life.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldState {
    /// Money is set aside and cannot be spent by anything else.
    Held,
    /// Some or all of it became a charge.
    Captured,
    /// It went back to the balance.
    Released,
}

impl Hold {
    /// Whether this hold may still be captured or released.
    ///
    /// A hold that has been captured or released is finished. Capturing it again would
    /// charge twice for one reservation, which is the mistake holds exist to prevent.
    #[must_use]
    pub const fn open(&self) -> bool {
        matches!(self.state, HoldState::Held)
    }
}

/// Capture `amount` of a hold: the journal record that turns a reservation into a
/// charge, plus the remainder that goes back.
///
/// # Errors
///
/// [`FinanceError::HoldClosed`] when the hold is finished, and
/// [`FinanceError::CaptureExceedsHold`] when more is captured than was held — a
/// reservation is a ceiling, not a suggestion.
pub fn capture(
    hold: &Hold,
    amount: Minor,
    revenue: &str,
    at_millis: i64,
) -> Result<JournalRecord, FinanceError> {
    if !hold.open() {
        return Err(FinanceError::HoldClosed);
    }
    if amount.0 < 0 || amount > hold.amount {
        return Err(FinanceError::CaptureExceedsHold);
    }
    let record = JournalRecord {
        idempotency_key: format!("capture:{}", hold.idempotency_key),
        at_millis,
        entries: vec![
            Entry {
                account: hold.account.clone(),
                amount: Minor(amount.0.checked_neg().ok_or(FinanceError::Overflow)?),
            },
            Entry {
                account: revenue.to_owned(),
                amount,
            },
        ],
        price_version: None,
    };
    if !record.balances() {
        return Err(FinanceError::Unbalanced);
    }
    Ok(record)
}

/// Refund a settled charge.
///
/// A refund is a new balanced record, never an edit of the original: the journal is
/// append-only, and a charge that was made and then returned is two facts, not none.
///
/// # Errors
///
/// [`FinanceError::Unbalanced`] if the construction is wrong, [`FinanceError::Overflow`]
/// on an impossible amount.
pub fn refund(
    settled: &JournalRecord,
    reason: &str,
    at_millis: i64,
) -> Result<JournalRecord, FinanceError> {
    let mut entries = Vec::with_capacity(settled.entries.len());
    for entry in &settled.entries {
        entries.push(Entry {
            account: entry.account.clone(),
            amount: Minor(entry.amount.0.checked_neg().ok_or(FinanceError::Overflow)?),
        });
    }
    let record = JournalRecord {
        idempotency_key: format!("refund:{}:{reason}", settled.idempotency_key),
        at_millis,
        entries,
        price_version: settled.price_version.clone(),
    };
    if !record.balances() {
        return Err(FinanceError::Unbalanced);
    }
    Ok(record)
}

/// What to do about a creature that cannot pay.
///
/// The steps escalate, and each one is reversible by paying: nothing here destroys a
/// workload, and every step after `Notify` goes through ordinary authorized VMM
/// operations so it is auditable.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    /// Enough balance: nothing to do.
    None,
    /// Below zero, inside the grace period: tell them.
    Notify,
    /// Grace is over: pause the workloads. Memory is preserved, so paying resumes
    /// exactly where they were.
    Pause,
    /// Paused and still unpaid past the pause window: stop them. State on disk
    /// survives; the processes do not.
    Stop,
}

/// How long enforcement waits at each step.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnforcementPolicy {
    /// How long a negative balance is tolerated before anything happens.
    pub grace_millis: i64,
    /// How long a workload stays paused before it is stopped.
    pub pause_millis: i64,
}

/// Decide enforcement from a balance and how long it has been negative.
///
/// Suspension is never immediate: a balance that dips below zero between a charge and
/// a top-up is normal, and stopping someone's workloads for it would be worse than
/// carrying the debt for an hour. Pausing before stopping means paying up restores the
/// running process rather than restarting it.
#[must_use]
pub fn enforcement(
    balance: Minor,
    negative_for_millis: i64,
    policy: EnforcementPolicy,
) -> Enforcement {
    if balance.0 >= 0 {
        return Enforcement::None;
    }
    if negative_for_millis < policy.grace_millis {
        return Enforcement::Notify;
    }
    if negative_for_millis < policy.grace_millis.saturating_add(policy.pause_millis) {
        return Enforcement::Pause;
    }
    Enforcement::Stop
}

/// A difference between what was metered, what was priced, and what was settled.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Discrepancy {
    /// An interval exists with no journal record.
    Unsettled { settlement_key: String },
    /// A journal record exists whose settlement key matches no interval.
    Unexplained { idempotency_key: String },
    /// A charge names a price version that is no longer published.
    UnknownPrice {
        idempotency_key: String,
        price_version: String,
    },
    /// A settled amount differs from what its interval prices to now.
    Mispriced {
        settlement_key: String,
        settled: Minor,
        recomputed: Minor,
    },
}

/// The corrective entry a discrepancy calls for, if any.
///
/// **Nothing here repairs automatically.** A corrective entry is proposed for a person
/// to review, because the alternative — a reconciler that silently moves money — turns
/// one bad charge into an unauditable series of them. An `Unexplained` record has no
/// proposal at all: money that arrived from nowhere is a question, not an arithmetic
/// problem.
///
/// # Errors
///
/// [`FinanceError::Overflow`] on an impossible amount.
pub fn corrective_entry(
    discrepancy: &Discrepancy,
    wallet: &str,
    revenue: &str,
    at_millis: i64,
) -> Result<Option<JournalRecord>, FinanceError> {
    match discrepancy {
        // Settling it is the ordinary path, not a correction.
        Discrepancy::Unsettled { .. } => Ok(None),
        // A record nobody can explain is escalated, never adjusted away.
        Discrepancy::Unexplained { .. } => Ok(None),
        // A price that no longer exists cannot be recomputed against.
        Discrepancy::UnknownPrice { .. } => Ok(None),
        Discrepancy::Mispriced {
            settlement_key,
            settled,
            recomputed,
        } => {
            let difference = recomputed.checked_sub(*settled)?;
            if difference.0 == 0 {
                return Ok(None);
            }
            let record = JournalRecord {
                idempotency_key: format!("correction:{settlement_key}"),
                at_millis,
                entries: vec![
                    Entry {
                        account: wallet.to_owned(),
                        amount: Minor(difference.0.checked_neg().ok_or(FinanceError::Overflow)?),
                    },
                    Entry {
                        account: revenue.to_owned(),
                        amount: difference,
                    },
                ],
                price_version: None,
            };
            if !record.balances() {
                return Err(FinanceError::Unbalanced);
            }
            Ok(Some(record))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FinanceError {
    #[error("the amount does not fit")]
    Overflow,
    #[error("two samples of different workloads are not an interval")]
    DifferentWorkloads,
    #[error("a sample must be newer than the one before it")]
    OutOfOrder,
    #[error("the price list does not price {0:?}")]
    UnpricedDimension(Dimension),
    #[error("a journal record's entries must sum to zero")]
    Unbalanced,
    #[error("the rate for {0:?} is zero, which is a silent free ride")]
    ZeroRate(Dimension),
    #[error("{0:?} may not be priced")]
    UnbillableDimension(Dimension),
    #[error("the hold is already captured or released")]
    HoldClosed,
    #[error("a reservation is a ceiling: more cannot be captured than was held")]
    CaptureExceedsHold,
}

#[cfg(test)]
mod golden;
#[cfg(test)]
mod tests;
