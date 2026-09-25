//! Metering and the ledger on PostgreSQL (Phase 8).
//!
//! The guarantee the whole phase rests on is a primary key: the settlement identity
//! `(workload, interval start, provider sample)` is the journal record's idempotency
//! key, so a duplicate collection, a retried message, or a crash between committing
//! and recording that it committed all land on the same row.

use aseman_domain::Uuid;
use aseman_domain::finance::{JournalRecord, Minor, PriceList, UsageInterval, UsageSample};
use aseman_ports::finance::{Ledger, PricingStore, UsageStore};
use aseman_ports::{PortError, PortResult};
use postgres::NoTls;
use r2d2::{Pool, PooledConnection};
use r2d2_postgres::PostgresConnectionManager;

/// Idempotent schema migration owned by the ledger provider.
pub const FINANCE_MIGRATION: &str = include_str!("../migrations/0001_finance.sql");

type Connection = PooledConnection<PostgresConnectionManager<NoTls>>;

fn failed(error: impl std::fmt::Display) -> PortError {
    PortError::Failed(error.to_string())
}

fn db(error: postgres::Error) -> PortError {
    match error.code() {
        Some(code) if *code == postgres::error::SqlState::UNIQUE_VIOLATION => PortError::Conflict,
        _ if error.is_closed() => PortError::Unavailable("postgres"),
        _ => failed(error),
    }
}

/// Metering and ledger storage.
pub struct PostgresFinance {
    pool: Pool<PostgresConnectionManager<NoTls>>,
}

impl PostgresFinance {
    #[must_use]
    pub fn new(pool: Pool<PostgresConnectionManager<NoTls>>) -> Self {
        Self { pool }
    }

    /// Connect with an already-parsed configuration.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the pool cannot be built.
    pub fn connect_config(config: postgres::Config, max_size: u32) -> PortResult<Self> {
        let pool = Pool::builder()
            .max_size(max_size.max(1))
            .build(PostgresConnectionManager::new(config, NoTls))
            .map_err(|_| PortError::Unavailable("finance database"))?;
        Ok(Self { pool })
    }

    /// Create the finance tables. Idempotent.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub fn migrate(&self) -> PortResult<()> {
        self.connection()?
            .batch_execute(FINANCE_MIGRATION)
            .map_err(db)
    }

    fn connection(&self) -> PortResult<Connection> {
        self.pool
            .get()
            .map_err(|_| PortError::Unavailable("postgres"))
    }
}

impl UsageStore for PostgresFinance {
    fn record_sample(&self, sample: &UsageSample) -> PortResult<()> {
        let mut connection = self.connection()?;
        let text = serde_json::to_string(sample).map_err(failed)?;
        // A repeated collection is refused here, so it can never become a second
        // interval and therefore never a second charge.
        let inserted = connection
            .execute(
                "INSERT INTO aseman_core.usage_sample \
                   (workload_id, provider_sample_id, provider, collected_at_millis, sample) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                &[
                    &sample.workload_id,
                    &sample.provider_sample_id,
                    &sample.provider,
                    &sample.collected_at_millis,
                    &text,
                ],
            )
            .map_err(db)?;
        if inserted == 0 {
            return Err(PortError::Conflict);
        }
        Ok(())
    }

    fn previous_sample(&self, workload: Uuid, at_millis: i64) -> PortResult<Option<UsageSample>> {
        let mut connection = self.connection()?;
        connection
            .query_opt(
                "SELECT sample FROM aseman_core.usage_sample \
                 WHERE workload_id = $1 AND collected_at_millis < $2 \
                 ORDER BY collected_at_millis DESC LIMIT 1",
                &[&workload, &at_millis],
            )
            .map_err(db)?
            .map(|row| {
                let text: String = row.get("sample");
                serde_json::from_str(&text).map_err(failed)
            })
            .transpose()
    }

    fn record_interval(&self, interval: &UsageInterval) -> PortResult<()> {
        let mut connection = self.connection()?;
        let text = serde_json::to_string(interval).map_err(failed)?;
        let inserted = connection
            .execute(
                "INSERT INTO aseman_core.usage_interval \
                   (settlement_key, workload_id, interval_start_millis, interval_end_millis, interval) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                &[
                    &interval.settlement_key(),
                    &interval.workload_id,
                    &interval.interval_start_millis,
                    &interval.interval_end_millis,
                    &text,
                ],
            )
            .map_err(db)?;
        if inserted == 0 {
            return Err(PortError::Conflict);
        }
        Ok(())
    }

    fn unsettled(&self, limit: usize) -> PortResult<Vec<UsageInterval>> {
        let mut connection = self.connection()?;
        // An interval is unsettled when no journal record carries its settlement key.
        let rows = connection
            .query(
                "SELECT interval FROM aseman_core.usage_interval AS usage \
                 WHERE NOT EXISTS ( \
                   SELECT 1 FROM aseman_core.journal_record AS journal \
                   WHERE journal.idempotency_key = usage.settlement_key \
                 ) \
                 ORDER BY usage.interval_start_millis, usage.settlement_key LIMIT $1",
                &[&i64::try_from(limit.max(1)).map_err(failed)?],
            )
            .map_err(db)?;
        rows.iter()
            .map(|row| {
                let text: String = row.get("interval");
                serde_json::from_str(&text).map_err(failed)
            })
            .collect()
    }
}

impl PricingStore for PostgresFinance {
    fn price_lists(&self) -> PortResult<Vec<PriceList>> {
        let mut connection = self.connection()?;
        let rows = connection
            .query(
                "SELECT list FROM aseman_core.price_list ORDER BY effective_from_millis, version",
                &[],
            )
            .map_err(db)?;
        rows.iter()
            .map(|row| {
                let text: String = row.get("list");
                serde_json::from_str(&text).map_err(failed)
            })
            .collect()
    }

    fn publish(&self, list: &PriceList) -> PortResult<()> {
        list.validate().map_err(|error| match error {
            // A price nobody can be charged at is a configuration mistake, and the
            // operator hears about it at publication rather than at reconciliation.
            aseman_domain::finance::FinanceError::ZeroRate(_)
            | aseman_domain::finance::FinanceError::UnbillableDimension(_) => {
                PortError::Denied("the price list is not chargeable")
            }
            other => failed(other),
        })?;
        let mut connection = self.connection()?;
        let text = serde_json::to_string(list).map_err(failed)?;
        // A published price is never edited: charges refer to it by version.
        let inserted = connection
            .execute(
                "INSERT INTO aseman_core.price_list (version, effective_from_millis, list) \
                 VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
                &[&list.version, &list.effective_from_millis, &text],
            )
            .map_err(db)?;
        if inserted == 0 {
            return Err(PortError::Conflict);
        }
        Ok(())
    }
}

impl Ledger for PostgresFinance {
    fn commit(&self, record: &JournalRecord) -> PortResult<()> {
        if !record.balances() {
            return Err(PortError::Denied("a journal record must balance"));
        }
        let mut connection = self.connection()?;
        let mut transaction = connection.transaction().map_err(db)?;
        let text = serde_json::to_string(record).map_err(failed)?;
        // Committing the same key twice is success. This is the whole mechanism: a
        // retry after a crash lands here and changes nothing.
        let inserted = transaction
            .execute(
                "INSERT INTO aseman_core.journal_record \
                   (idempotency_key, at_millis, price_version, record) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT DO NOTHING",
                &[
                    &record.idempotency_key,
                    &record.at_millis,
                    &record.price_version,
                    &text,
                ],
            )
            .map_err(db)?;
        if inserted == 0 {
            transaction.commit().map_err(db)?;
            return Ok(());
        }
        // Entries go in the same transaction as the record: a balance is never the
        // sum of half a settlement.
        for (ordinal, entry) in record.entries.iter().enumerate() {
            transaction
                .execute(
                    "INSERT INTO aseman_core.journal_entry \
                       (idempotency_key, ordinal, account, amount) VALUES ($1, $2, $3, $4)",
                    &[
                        &record.idempotency_key,
                        &i32::try_from(ordinal).map_err(failed)?,
                        &entry.account,
                        &entry.amount.0,
                    ],
                )
                .map_err(db)?;
        }
        transaction.commit().map_err(db)?;
        Ok(())
    }

    fn record(&self, idempotency_key: &str) -> PortResult<Option<JournalRecord>> {
        let mut connection = self.connection()?;
        connection
            .query_opt(
                "SELECT record FROM aseman_core.journal_record WHERE idempotency_key = $1",
                &[&idempotency_key],
            )
            .map_err(db)?
            .map(|row| {
                let text: String = row.get("record");
                serde_json::from_str(&text).map_err(failed)
            })
            .transpose()
    }

    fn balance(&self, account: &str) -> PortResult<Minor> {
        let mut connection = self.connection()?;
        let total: Option<i64> = connection
            .query_one(
                "SELECT SUM(amount)::bigint AS total FROM aseman_core.journal_entry \
                 WHERE account = $1",
                &[&account],
            )
            .map_err(db)?
            .get("total");
        Ok(Minor(total.unwrap_or(0)))
    }

    fn settlements(&self, workload: Uuid, limit: usize) -> PortResult<Vec<JournalRecord>> {
        let mut connection = self.connection()?;
        let rows = connection
            .query(
                "SELECT journal.record FROM aseman_core.journal_record AS journal \
                 JOIN aseman_core.usage_interval AS usage \
                   ON usage.settlement_key = journal.idempotency_key \
                 WHERE usage.workload_id = $1 \
                 ORDER BY journal.at_millis, journal.idempotency_key LIMIT $2",
                &[&workload, &i64::try_from(limit.max(1)).map_err(failed)?],
            )
            .map_err(db)?;
        rows.iter()
            .map(|row| {
                let text: String = row.get("record");
                serde_json::from_str(&text).map_err(failed)
            })
            .collect()
    }
}
