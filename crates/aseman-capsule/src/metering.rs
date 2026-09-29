//! The metering ports on the storage module (ADR 0038, A801): usage samples and
//! intervals (`core.usage_sample`, `core.usage_interval`), price lists
//! (`core.price_list`), and the double-entry journal (`core.journal_record` with its
//! `core.journal_entry` rows, written in one transaction so a balance is never the
//! sum of half a settlement).

use aseman_domain::Uuid;
use aseman_domain::finance::{JournalRecord, Minor, PriceList, UsageInterval, UsageSample};
use aseman_ports::finance::{Ledger, PricingStore, UsageStore};
use aseman_ports::{PortError, PortResult};
use aseman_storage::client::core::{
    journal_entry, journal_record, price_list, usage_interval, usage_sample,
};
use aseman_storage::{FindMany, Models, Storage, StorageError};

use crate::auto::AutoCommit;

/// Intervals examined per page while looking for unsettled ones.
const PAGE: u64 = 256;

/// Metering and the ledger in the node's storage.
#[derive(Clone)]
pub struct StorageMetering(AutoCommit);

impl StorageMetering {
    #[must_use]
    pub fn new(storage: Storage) -> Self {
        Self(AutoCommit(storage))
    }
}

fn encoded<T: serde::Serialize>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|error| StorageError::invalid(error.to_string()))
}

fn decoded<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, StorageError> {
    serde_json::from_str(text).map_err(|error| StorageError::invalid(error.to_string()))
}

fn sample_key(workload: Uuid, provider_sample_id: &str) -> String {
    format!("{workload}::{provider_sample_id}")
}

impl UsageStore for StorageMetering {
    fn record_sample(&self, sample: &UsageSample) -> PortResult<()> {
        let key = sample_key(sample.workload_id, &sample.provider_sample_id);
        // A repeated collection is refused, so it can never become a second interval
        // and therefore never a second charge.
        self.0.decide(|trx| {
            if trx
                .usage_sample()
                .find_unique(usage_sample::by_key(key.clone()))?
                .is_some()
            {
                return Ok(Err(PortError::Conflict));
            }
            trx.usage_sample().create(usage_sample::Create {
                key: key.clone(),
                workload_id: sample.workload_id.to_string(),
                provider_sample_id: sample.provider_sample_id.clone(),
                provider: sample.provider.clone(),
                collected_at_millis: sample.collected_at_millis,
                sample: encoded(sample)?,
            })?;
            Ok(Ok(()))
        })
    }

    fn previous_sample(&self, workload: Uuid, at_millis: i64) -> PortResult<Option<UsageSample>> {
        self.0.read(|trx| {
            trx.usage_sample()
                .find_first(
                    FindMany::filter(
                        usage_sample::workload_id()
                            .eq(workload.to_string())
                            .and(usage_sample::collected_at_millis().lt(at_millis)),
                    )
                    .order_by(usage_sample::collected_at_millis().desc()),
                )?
                .map(|row| decoded(&row.sample))
                .transpose()
        })
    }

    fn record_interval(&self, interval: &UsageInterval) -> PortResult<()> {
        let key = interval.settlement_key();
        self.0.decide(|trx| {
            if trx
                .usage_interval()
                .find_unique(usage_interval::by_key(key.clone()))?
                .is_some()
            {
                return Ok(Err(PortError::Conflict));
            }
            trx.usage_interval().create(usage_interval::Create {
                key: key.clone(),
                workload_id: interval.workload_id.to_string(),
                interval_start_millis: interval.interval_start_millis,
                interval_end_millis: interval.interval_end_millis,
                interval: encoded(interval)?,
            })?;
            Ok(Ok(()))
        })
    }

    fn unsettled(&self, limit: usize) -> PortResult<Vec<UsageInterval>> {
        // An interval is unsettled when no journal record carries its settlement key.
        let limit = limit.max(1);
        self.0.read(|trx| {
            let mut found = Vec::new();
            let mut skip = 0;
            loop {
                let page = trx.usage_interval().find_many(
                    FindMany::default()
                        .order_by(usage_interval::interval_start_millis().asc())
                        .order_by(usage_interval::key().asc())
                        .skip(skip)
                        .take(PAGE),
                )?;
                let exhausted = (page.len() as u64) < PAGE;
                skip += page.len() as u64;
                for row in page {
                    if trx
                        .journal_record()
                        .find_unique(journal_record::by_key(row.key.clone()))?
                        .is_none()
                    {
                        found.push(decoded(&row.interval)?);
                        if found.len() == limit {
                            return Ok(found);
                        }
                    }
                }
                if exhausted {
                    return Ok(found);
                }
            }
        })
    }
}

impl PricingStore for StorageMetering {
    fn price_lists(&self) -> PortResult<Vec<PriceList>> {
        self.0.read(|trx| {
            trx.price_list()
                .find_many(
                    FindMany::default()
                        .order_by(price_list::effective_from_millis().asc())
                        .order_by(price_list::key().asc()),
                )?
                .iter()
                .map(|row| decoded(&row.list))
                .collect()
        })
    }

    fn publish(&self, list: &PriceList) -> PortResult<()> {
        list.validate().map_err(|error| match error {
            // A price nobody can be charged at is a configuration mistake, and the
            // operator hears about it at publication rather than at reconciliation.
            aseman_domain::finance::FinanceError::ZeroRate(_)
            | aseman_domain::finance::FinanceError::UnbillableDimension(_) => {
                PortError::Denied("the price list is not chargeable")
            }
            other => PortError::failed(other),
        })?;
        // A published price is never edited: charges refer to it by version.
        self.0.decide(|trx| {
            if trx
                .price_list()
                .find_unique(price_list::by_key(list.version.clone()))?
                .is_some()
            {
                return Ok(Err(PortError::Conflict));
            }
            trx.price_list().create(price_list::Create {
                key: list.version.clone(),
                effective_from_millis: list.effective_from_millis,
                list: encoded(list)?,
            })?;
            Ok(Ok(()))
        })
    }
}

impl Ledger for StorageMetering {
    fn commit(&self, record: &JournalRecord) -> PortResult<()> {
        if !record.balances() {
            return Err(PortError::Denied("a journal record must balance"));
        }
        self.0.decide(|trx| {
            // Committing the same key twice is success: a retry after a crash lands
            // here and changes nothing.
            if trx
                .journal_record()
                .find_unique(journal_record::by_key(record.idempotency_key.clone()))?
                .is_some()
            {
                return Ok(Ok(()));
            }
            trx.journal_record().create(journal_record::Create {
                key: record.idempotency_key.clone(),
                at_millis: record.at_millis,
                price_version: record.price_version.clone(),
                record: encoded(record)?,
            })?;
            for (ordinal, entry) in record.entries.iter().enumerate() {
                trx.journal_entry().create(journal_entry::Create {
                    key: format!("{}::{ordinal}", record.idempotency_key),
                    record_key: record.idempotency_key.clone(),
                    ordinal: i64::try_from(ordinal)
                        .map_err(|error| StorageError::invalid(error.to_string()))?,
                    account: entry.account.clone(),
                    amount: entry.amount.0,
                })?;
            }
            Ok(Ok(()))
        })
    }

    fn record(&self, idempotency_key: &str) -> PortResult<Option<JournalRecord>> {
        self.0.read(|trx| {
            trx.journal_record()
                .find_unique(journal_record::by_key(idempotency_key))?
                .map(|row| decoded(&row.record))
                .transpose()
        })
    }

    fn balance(&self, account: &str) -> PortResult<Minor> {
        self.0.read(|trx| {
            Ok(Minor(
                trx.journal_entry()
                    .find_where(journal_entry::account().eq(account))?
                    .iter()
                    .map(|entry| entry.amount)
                    .sum(),
            ))
        })
    }

    fn settlements(&self, workload: Uuid, limit: usize) -> PortResult<Vec<JournalRecord>> {
        self.0.read(|trx| {
            let keys: Vec<String> = trx
                .usage_interval()
                .find_where(usage_interval::workload_id().eq(workload.to_string()))?
                .into_iter()
                .map(|interval| interval.key)
                .collect();
            if keys.is_empty() {
                return Ok(Vec::new());
            }
            trx.journal_record()
                .find_many(
                    FindMany::filter(journal_record::key().is_in(keys))
                        .order_by(journal_record::at_millis().asc())
                        .order_by(journal_record::key().asc())
                        .take(limit.max(1) as u64),
                )?
                .iter()
                .map(|row| decoded(&row.record))
                .collect()
        })
    }
}
