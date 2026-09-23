//! Finance ports (Phase 8): metering, pricing, the ledger, and enforcement.
//!
//! The ports are separate because the concerns are. A pricing change must not be able
//! to touch the ledger; a metering outage must not be able to invent a charge.

use aseman_domain::Uuid;
use aseman_domain::finance::{Charge, JournalRecord, Minor, PriceList, UsageInterval, UsageSample};

use crate::PortResult;

/// Where raw provider samples are kept.
///
/// Raw samples are retained, not just the deltas: a charge must be explainable years
/// later from the reading that produced it.
pub trait UsageStore: Send + Sync {
    /// Record a sample. `Conflict` when this provider sample is already stored, so a
    /// repeated collection cannot become a second interval.
    ///
    /// # Errors
    ///
    /// When the store refuses or is unreachable.
    fn record_sample(&self, sample: &UsageSample) -> PortResult<()>;

    /// The newest sample before `at_millis` for a workload, which is what the next
    /// interval is measured from.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn previous_sample(&self, workload: Uuid, at_millis: i64) -> PortResult<Option<UsageSample>>;

    /// Record a derived interval. `Conflict` when its settlement key already exists.
    ///
    /// # Errors
    ///
    /// When the store refuses or is unreachable.
    fn record_interval(&self, interval: &UsageInterval) -> PortResult<()>;

    /// Intervals that have no settlement yet, oldest first.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn unsettled(&self, limit: usize) -> PortResult<Vec<UsageInterval>>;
}

/// The versioned price lists.
pub trait PricingStore: Send + Sync {
    /// Every price list, oldest first.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn price_lists(&self) -> PortResult<Vec<PriceList>>;

    /// Publish a price list. `Conflict` when that version already exists: a published
    /// price is never edited, because charges refer to it.
    ///
    /// # Errors
    ///
    /// When the store refuses or is unreachable.
    fn publish(&self, list: &PriceList) -> PortResult<()>;
}

/// The append-only double-entry ledger.
pub trait Ledger: Send + Sync {
    /// Commit a balanced record atomically, keyed by its idempotency key.
    ///
    /// Committing the same key twice is **success, not a second record**. That is the
    /// whole mechanism: a retry after a crash, an outage, or a duplicated message
    /// lands on the same key and changes nothing.
    ///
    /// # Errors
    ///
    /// [`crate::PortError::Denied`] when the record does not balance; otherwise when
    /// the store is unreachable.
    fn commit(&self, record: &JournalRecord) -> PortResult<()>;

    /// Whether a key has been committed, and the record if so.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn record(&self, idempotency_key: &str) -> PortResult<Option<JournalRecord>>;

    /// An account's balance: the sum of its entries.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn balance(&self, account: &str) -> PortResult<Minor>;

    /// Every entry of a settlement, for reconciliation.
    ///
    /// # Errors
    ///
    /// When the store is unreachable.
    fn settlements(&self, workload: Uuid, limit: usize) -> PortResult<Vec<JournalRecord>>;
}

/// What a settlement pass found, for an operator rather than for automatic repair.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Reconciliation {
    /// Intervals with no ledger record.
    pub unsettled: Vec<String>,
    /// Ledger records with no interval behind them.
    pub unexplained: Vec<String>,
    /// Charges whose price version is no longer published.
    pub unpriced: Vec<String>,
}

impl Reconciliation {
    /// Whether everything traces.
    #[must_use]
    pub fn clean(&self) -> bool {
        self.unsettled.is_empty() && self.unexplained.is_empty() && self.unpriced.is_empty()
    }
}

/// Settling one interval, as the metering loop does it.
pub trait Settlement: Send + Sync {
    /// Price and settle an interval, or report that it already was.
    ///
    /// # Errors
    ///
    /// When a store is unreachable or the interval cannot be priced.
    fn settle(&self, interval: &UsageInterval) -> PortResult<Charge>;
}
