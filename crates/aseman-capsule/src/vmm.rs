//! The VMM service's stores (A501, A503) on the storage module (ADR 0038): workloads
//! (`core.vmm_workload`), operations (`core.vmm_operation`), idempotency claims
//! (`core.vmm_idempotency`), and the event log (`core.vmm_event`, sequenced by the
//! `core.counter` rows [`LAST_SEQUENCE`] and [`TRUNCATED_THROUGH`]).
//!
//! Records are stored whole as JSON next to the fields that queries and
//! compare-and-set need. The workload record holds the write-only bootstrap
//! credential, which the backend needs to restart an instance; the API never returns
//! it (`WorkloadSpec::redacted`).

use aseman_domain::vmm::{OperationRecord, WorkloadEventRecord, WorkloadRecord};
use aseman_domain::{OperationId, OperationState, WorkloadId};
use aseman_ports::vmm::{
    EventBatch, IdempotencyClaim, IdempotencyStore, OperationFilter, Page, ReplayableResponse,
    VmmEventLog, VmmOperationStore, VmmWorkloadStore, WorkloadFilter,
};
use aseman_ports::{PortError, PortResult};
use aseman_storage::client::core::{
    counter, vmm_event, vmm_idempotency, vmm_operation, vmm_workload,
};
use aseman_storage::{FindMany, Models, Storage, StorageError, Trx, Where};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::auto::AutoCommit;

/// The counter holding the last event sequence handed out.
pub const LAST_SEQUENCE: &str = "vmm.event.last_sequence";
/// The counter holding the highest event sequence truncation has dropped.
pub const TRUNCATED_THROUGH: &str = "vmm.event.truncated_through";

/// The VMM service's stores in its storage.
#[derive(Clone)]
pub struct StorageVmmStore(AutoCommit);

impl StorageVmmStore {
    #[must_use]
    pub fn new(storage: Storage) -> Self {
        Self(AutoCommit(storage))
    }
}

fn invalid(error: impl std::fmt::Display) -> StorageError {
    StorageError::invalid(error.to_string())
}

fn encode<T: Serialize>(value: &T) -> Result<serde_json::Value, StorageError> {
    serde_json::to_value(value).map_err(invalid)
}

fn decode<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, StorageError> {
    serde_json::from_value(value).map_err(invalid)
}

fn state_text<T: Serialize>(state: &T) -> Result<String, StorageError> {
    match encode(state)? {
        serde_json::Value::String(text) => Ok(text),
        other => Err(invalid(format!("not a state: {other}"))),
    }
}

fn cursor_key(cursor: Option<&str>) -> PortResult<Option<String>> {
    cursor
        .map(|cursor| {
            uuid::Uuid::parse_str(cursor)
                .map(|id| id.to_string())
                .map_err(|_| PortError::Failed("invalid cursor".to_owned()))
        })
        .transpose()
}

/// One more row than a page holds, to learn whether another page follows.
fn probe(limit: usize) -> u64 {
    limit.max(1) as u64 + 1
}

fn page_of<T>(mut items: Vec<T>, limit: usize, key: impl Fn(&T) -> String) -> Page<T> {
    let more = items.len() > limit.max(1);
    items.truncate(limit.max(1));
    Page {
        next_cursor: if more { items.last().map(key) } else { None },
        items,
    }
}

fn signed(value: u64) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(invalid)
}

fn observed_text(record: &WorkloadRecord) -> Result<Option<String>, StorageError> {
    record
        .observed
        .as_ref()
        .map(|observed| state_text(&observed.state))
        .transpose()
}

fn counter_value(trx: &Trx, key: &str) -> Result<i64, StorageError> {
    Ok(trx
        .counter()
        .find_unique(counter::by_key(key))?
        .map_or(0, |row| row.value))
}

/// Write `value` to the counter `key`, created on first use. The write carries the
/// revision read in this transaction, so concurrent writers serialize on it.
fn set_counter(trx: &Trx, key: &str, value: i64) -> Result<(), StorageError> {
    trx.counter().upsert(
        counter::by_key(key),
        counter::Create {
            key: key.to_owned(),
            value,
        },
        counter::update().value(value),
    )?;
    Ok(())
}

impl VmmWorkloadStore for StorageVmmStore {
    fn workload(&self, owner: &str, id: WorkloadId) -> PortResult<Option<WorkloadRecord>> {
        self.0.read(|trx| {
            trx.vmm_workload()
                .find_unique(vmm_workload::by_key(id.to_string()))?
                .filter(|row| row.owner == owner)
                .map(|row| decode(row.record))
                .transpose()
        })
    }

    fn workloads(
        &self,
        owner: &str,
        filter: &WorkloadFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> PortResult<Page<WorkloadRecord>> {
        let after = cursor_key(cursor)?;
        let rows = self.0.read(|trx| {
            let mut conditions = vec![vmm_workload::owner().eq(owner)];
            if let Some(after) = &after {
                conditions.push(vmm_workload::key().gt(after.clone()));
            }
            if let Some(creature) = filter.creature_id {
                conditions.push(vmm_workload::creature_id().eq(creature.to_string()));
            }
            if let Some(state) = &filter.observed_state {
                conditions.push(vmm_workload::observed_state().eq(state_text(state)?));
            }
            trx.vmm_workload()
                .find_many(
                    FindMany {
                        filter: Where::all(conditions),
                        ..FindMany::default()
                    }
                    .order_by(vmm_workload::key().asc())
                    .take(probe(limit)),
                )?
                .into_iter()
                .map(|row| decode(row.record))
                .collect::<Result<Vec<WorkloadRecord>, _>>()
        })?;
        Ok(page_of(rows, limit, |record| record.id.to_string()))
    }

    fn all_workloads(
        &self,
        cursor: Option<&str>,
        limit: usize,
    ) -> PortResult<Page<WorkloadRecord>> {
        let after = cursor_key(cursor)?;
        let rows = self.0.read(|trx| {
            trx.vmm_workload()
                .find_many(
                    FindMany {
                        filter: after.clone().map(|after| vmm_workload::key().gt(after)),
                        ..FindMany::default()
                    }
                    .order_by(vmm_workload::key().asc())
                    .take(probe(limit)),
                )?
                .into_iter()
                .map(|row| decode(row.record))
                .collect::<Result<Vec<WorkloadRecord>, _>>()
        })?;
        Ok(page_of(rows, limit, |record| record.id.to_string()))
    }

    fn insert_workload(&self, record: &WorkloadRecord) -> PortResult<()> {
        self.0.decide(|trx| {
            trx.vmm_workload().create(vmm_workload::Create {
                key: record.id.to_string(),
                owner: record.owner.clone(),
                creature_id: record.labels.creature_id.to_string(),
                observed_state: observed_text(record)?,
                resource_version: signed(record.resource_version)?,
                record: encode(record)?,
            })?;
            Ok(Ok(()))
        })
    }

    fn replace_workload(&self, record: &WorkloadRecord, expected: u64) -> PortResult<()> {
        let expected = i64::try_from(expected).map_err(PortError::failed)?;
        self.0.decide(|trx| {
            let key = vmm_workload::by_key(record.id.to_string());
            let Some(current) = trx.vmm_workload().find_unique(key.clone())? else {
                return Ok(Err(PortError::NotFound));
            };
            if current.owner != record.owner || current.resource_version != expected {
                return Ok(Err(PortError::Conflict));
            }
            trx.vmm_workload().update(
                key,
                vmm_workload::update()
                    .creature_id(record.labels.creature_id.to_string())
                    .observed_state(observed_text(record)?)
                    .resource_version(signed(record.resource_version)?)
                    .record(encode(record)?),
            )?;
            Ok(Ok(()))
        })
    }
}

impl VmmOperationStore for StorageVmmStore {
    fn operation(&self, owner: &str, id: OperationId) -> PortResult<Option<OperationRecord>> {
        self.0.read(|trx| {
            trx.vmm_operation()
                .find_unique(vmm_operation::by_key(id.to_string()))?
                .filter(|row| row.owner == owner)
                .map(|row| decode(row.record))
                .transpose()
        })
    }

    fn operations(
        &self,
        owner: &str,
        filter: &OperationFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> PortResult<Page<OperationRecord>> {
        let after = cursor_key(cursor)?;
        let rows = self.0.read(|trx| {
            let mut conditions = vec![vmm_operation::owner().eq(owner)];
            if let Some(workload) = filter.workload_id {
                conditions.push(vmm_operation::workload_id().eq(workload.to_string()));
            }
            if let Some(state) = &filter.state {
                conditions.push(vmm_operation::state().eq(state_text(state)?));
            }
            if let Some(after) = &after {
                // Newest first: the page after a cursor holds what sorts below it.
                let Some(last) = trx
                    .vmm_operation()
                    .find_unique(vmm_operation::by_key(after.clone()))?
                else {
                    return Ok(Vec::new());
                };
                conditions.push(Where::Or(vec![
                    vmm_operation::created_at_millis().lt(last.created_at_millis),
                    vmm_operation::created_at_millis()
                        .eq(last.created_at_millis)
                        .and(vmm_operation::key().lt(last.key)),
                ]));
            }
            trx.vmm_operation()
                .find_many(
                    FindMany {
                        filter: Where::all(conditions),
                        ..FindMany::default()
                    }
                    .order_by(vmm_operation::created_at_millis().desc())
                    .order_by(vmm_operation::key().desc())
                    .take(probe(limit)),
                )?
                .into_iter()
                .map(|row| decode(row.record))
                .collect::<Result<Vec<OperationRecord>, _>>()
        })?;
        Ok(page_of(rows, limit, |record| record.id.to_string()))
    }

    fn insert_operation(&self, record: &OperationRecord) -> PortResult<()> {
        self.0.decide(|trx| {
            trx.vmm_operation().create(vmm_operation::Create {
                key: record.id.to_string(),
                owner: record.owner.clone(),
                workload_id: record.workload_id.map(|id| id.to_string()),
                state: state_text(&record.state)?,
                created_at_millis: record.created_at_millis,
                record: encode(record)?,
            })?;
            Ok(Ok(()))
        })
    }

    fn replace_operation(
        &self,
        record: &OperationRecord,
        expected: OperationState,
    ) -> PortResult<()> {
        self.0.decide(|trx| {
            let key = vmm_operation::by_key(record.id.to_string());
            let Some(current) = trx.vmm_operation().find_unique(key.clone())? else {
                return Ok(Err(PortError::NotFound));
            };
            if current.owner != record.owner || current.state != state_text(&expected)? {
                return Ok(Err(PortError::Conflict));
            }
            trx.vmm_operation().update(
                key,
                vmm_operation::update()
                    .state(state_text(&record.state)?)
                    .record(encode(record)?),
            )?;
            Ok(Ok(()))
        })
    }

    fn unfinished_operations(&self, limit: usize) -> PortResult<Vec<OperationRecord>> {
        self.0.read(|trx| {
            trx.vmm_operation()
                .find_many(
                    FindMany::filter(vmm_operation::state().is_in([
                        state_text(&OperationState::Pending)?,
                        state_text(&OperationState::Running)?,
                    ]))
                    .order_by(vmm_operation::created_at_millis().asc())
                    .order_by(vmm_operation::key().asc())
                    .take(limit.max(1) as u64),
                )?
                .into_iter()
                .map(|row| decode(row.record))
                .collect()
        })
    }
}

fn claim_key(owner: &str, key: &str) -> String {
    format!("{owner}\u{1f}{key}")
}

impl IdempotencyStore for StorageVmmStore {
    fn claim(
        &self,
        owner: &str,
        key: &str,
        digest: [u8; 32],
        now_millis: i64,
        claim_ttl_millis: i64,
    ) -> PortResult<IdempotencyClaim> {
        let record = claim_key(owner, key);
        self.0.decide(|trx| {
            let Some(stored) = trx
                .vmm_idempotency()
                .find_unique(vmm_idempotency::by_key(record.clone()))?
            else {
                // Two first claims race on the unique key: the loser's commit
                // conflicts and it classifies the winner's claim.
                trx.vmm_idempotency().create(vmm_idempotency::Create {
                    key: record.clone(),
                    owner: owner.to_owned(),
                    request_key: key.to_owned(),
                    digest: digest.to_vec(),
                    claimed_at_millis: now_millis,
                    response_status: None,
                    response_body: None,
                    response_content_type: None,
                    response_location: None,
                })?;
                return Ok(Ok(IdempotencyClaim::Claimed));
            };
            if stored.digest != digest {
                return Ok(Ok(IdempotencyClaim::Mismatch));
            }
            let Some(status) = stored.response_status else {
                // An abandoned claim with the same request is taken over.
                if stored.claimed_at_millis <= now_millis - claim_ttl_millis {
                    trx.vmm_idempotency().update(
                        vmm_idempotency::by_key(record.clone()),
                        vmm_idempotency::update().claimed_at_millis(now_millis),
                    )?;
                    return Ok(Ok(IdempotencyClaim::Claimed));
                }
                return Ok(Ok(IdempotencyClaim::InProgress));
            };
            Ok(Ok(IdempotencyClaim::Completed(ReplayableResponse {
                status: u16::try_from(status).map_err(invalid)?,
                body: stored.response_body.unwrap_or_default(),
                content_type: stored.response_content_type.unwrap_or_default(),
                location: stored.response_location,
            })))
        })
    }

    fn complete(&self, owner: &str, key: &str, response: &ReplayableResponse) -> PortResult<()> {
        self.0.decide(|trx| {
            let updated = trx.vmm_idempotency().update(
                vmm_idempotency::by_key(claim_key(owner, key)),
                vmm_idempotency::update()
                    .response_status(Some(i64::from(response.status)))
                    .response_body(Some(response.body.clone()))
                    .response_content_type(Some(response.content_type.clone()))
                    .response_location(response.location.clone()),
            )?;
            Ok(updated.map(drop).ok_or(PortError::NotFound))
        })
    }

    fn release(&self, owner: &str, key: &str) -> PortResult<()> {
        self.0.decide(|trx| {
            let by = vmm_idempotency::by_key(claim_key(owner, key));
            if trx
                .vmm_idempotency()
                .find_unique(by.clone())?
                .is_some_and(|claim| claim.response_status.is_none())
            {
                trx.vmm_idempotency().delete(by)?;
            }
            Ok(Ok(()))
        })
    }

    fn purge_before(&self, cutoff_millis: i64) -> PortResult<u64> {
        self.0.decide(|trx| {
            Ok(Ok(trx.vmm_idempotency().delete_many(Some(
                vmm_idempotency::claimed_at_millis().lt(cutoff_millis),
            ))?))
        })
    }
}

impl VmmEventLog for StorageVmmStore {
    fn append(&self, event: &WorkloadEventRecord) -> PortResult<u64> {
        self.0.decide(|trx| {
            // Appends serialize on the sequence counter's revision.
            let sequence = counter_value(trx, LAST_SEQUENCE)? + 1;
            set_counter(trx, LAST_SEQUENCE, sequence)?;
            let mut event = event.clone();
            event.sequence = u64::try_from(sequence).map_err(invalid)?;
            trx.vmm_event().create(vmm_event::Create {
                key: format!("{sequence:020}"),
                sequence,
                owner: event.owner.clone(),
                workload_id: event.workload_id.to_string(),
                at_millis: event.at_millis,
                record: encode(&event)?,
            })?;
            Ok(Ok(event.sequence))
        })
    }

    fn events_after(
        &self,
        owner: &str,
        after: u64,
        workload: Option<WorkloadId>,
        limit: usize,
    ) -> PortResult<EventBatch> {
        let after = i64::try_from(after).map_err(PortError::failed)?;
        self.0.read(|trx| {
            if after < counter_value(trx, TRUNCATED_THROUGH)? {
                return Ok(EventBatch {
                    events: Vec::new(),
                    resync: true,
                });
            }
            let mut conditions = vec![
                vmm_event::owner().eq(owner),
                vmm_event::sequence().gt(after),
            ];
            if let Some(workload) = workload {
                conditions.push(vmm_event::workload_id().eq(workload.to_string()));
            }
            let events = trx
                .vmm_event()
                .find_many(
                    FindMany {
                        filter: Where::all(conditions),
                        ..FindMany::default()
                    }
                    .order_by(vmm_event::sequence().asc())
                    .take(limit.max(1) as u64),
                )?
                .into_iter()
                .map(|row| decode(row.record))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(EventBatch {
                events,
                resync: false,
            })
        })
    }

    fn truncate_before(&self, cutoff_millis: i64) -> PortResult<u64> {
        self.0.decide(|trx| {
            let expired = vmm_event::at_millis().lt(cutoff_millis);
            let Some(highest) = trx.vmm_event().find_first(
                FindMany::filter(expired.clone()).order_by(vmm_event::sequence().desc()),
            )?
            else {
                return Ok(Ok(0));
            };
            // Rewriting the sequence counter serializes with appends, so a sequence
            // committed later is never dropped.
            set_counter(trx, LAST_SEQUENCE, counter_value(trx, LAST_SEQUENCE)?)?;
            let truncated = counter_value(trx, TRUNCATED_THROUGH)?.max(highest.sequence);
            set_counter(trx, TRUNCATED_THROUGH, truncated)?;
            Ok(Ok(trx.vmm_event().delete_many(Some(expired))?))
        })
    }
}
