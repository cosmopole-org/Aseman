//! The durable realtime ports on the storage module (ADR 0038, A707): the event log
//! (`core.realtime_log_event`), its outbox (`core.realtime_outbox_entry`, written in
//! the same transaction as the event), and consumer checkpoints
//! (`core.realtime_checkpoint`).
//!
//! An outbox claim rewrites each entry with the revision it read, so two workers
//! never both claim an entry: the loser's commit conflicts and it claims again from
//! what is left, which is what `SKIP LOCKED` gave the PostgreSQL-only adapter.

use aseman_domain::Uuid;
use aseman_domain::realtime::{Checkpoint, Event, RetentionClass};
use aseman_ports::realtime::{CheckpointStore, Claim, EventLog, Outbox, Publication};
use aseman_ports::{PortError, PortResult};
use aseman_storage::client::core::{
    realtime_checkpoint, realtime_log_event, realtime_outbox_entry,
};
use aseman_storage::{FindMany, Models, Storage, StorageError, Where};

use crate::auto::AutoCommit;

/// Publication attempts before an event is a dead letter.
const MAX_ATTEMPTS: i64 = 8;
/// How long the time-bounded retention classes keep an event.
const TRANSIENT_MILLIS: i64 = 60 * 60 * 1000;
const STANDARD_MILLIS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The realtime log, outbox, and checkpoints in the node's storage.
#[derive(Clone)]
pub struct StorageRealtime(AutoCommit);

impl StorageRealtime {
    #[must_use]
    pub fn new(storage: Storage) -> Self {
        Self(AutoCommit(storage))
    }
}

fn retention_name(class: RetentionClass) -> &'static str {
    match class {
        RetentionClass::Transient => "transient",
        RetentionClass::Standard => "standard",
        RetentionClass::Durable => "durable",
    }
}

fn retention_class(name: &str) -> RetentionClass {
    match name {
        "transient" => RetentionClass::Transient,
        "durable" => RetentionClass::Durable,
        _ => RetentionClass::Standard,
    }
}

fn signed(value: u64) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|error| StorageError::invalid(error.to_string()))
}

fn uuid(text: &str) -> Result<Uuid, StorageError> {
    Uuid::parse_str(text).map_err(|error| StorageError::invalid(error.to_string()))
}

fn publication(row: realtime_log_event::RealtimeLogEvent) -> Result<Publication, StorageError> {
    Ok(Publication {
        event: Event {
            id: uuid(&row.key)?,
            stream: row.stream,
            creature_id: uuid(&row.creature_id)?,
            kind: row.event_kind,
            producer: row.producer,
            sequence: u64::try_from(row.sequence)
                .map_err(|error| StorageError::invalid(error.to_string()))?,
            at_millis: row.at_millis,
            payload_digest: row.payload_digest,
            retention: retention_class(&row.retention),
            version: row.version,
            idempotency_key: row.idempotency_key,
        },
        payload: row.payload,
    })
}

/// The events named by `ids`, in stream order.
fn events(trx: &aseman_storage::Trx, ids: &[String]) -> Result<Vec<Publication>, StorageError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    trx.realtime_log_event()
        .find_many(
            FindMany::filter(realtime_log_event::key().is_in(ids.iter().cloned()))
                .order_by(realtime_log_event::sequence().asc()),
        )?
        .into_iter()
        .map(publication)
        .collect()
}

impl EventLog for StorageRealtime {
    fn append(&self, published: &Publication) -> PortResult<()> {
        let event = &published.event;
        self.0.decide(|trx| {
            // The unique (stream, sequence) index refuses a racing repeat (the retry
            // then sees it taken); this check refuses a gap. Both matter: a gap makes a
            // consumer wait forever.
            let last = trx
                .realtime_log_event()
                .find_first(
                    FindMany::filter(realtime_log_event::stream().eq(event.stream.clone()))
                        .order_by(realtime_log_event::sequence().desc()),
                )?
                .map(|row| row.sequence);
            if signed(event.sequence)? != last.map_or(1, |last| last + 1) {
                return Ok(Err(PortError::Conflict));
            }
            trx.realtime_log_event()
                .create(realtime_log_event::Create {
                    key: event.id.to_string(),
                    stream: event.stream.clone(),
                    sequence: signed(event.sequence)?,
                    creature_id: event.creature_id.to_string(),
                    event_kind: event.kind.clone(),
                    producer: event.producer.clone(),
                    at_millis: event.at_millis,
                    payload_digest: event.payload_digest.clone(),
                    retention: retention_name(event.retention).to_owned(),
                    version: event.version.clone(),
                    idempotency_key: event.idempotency_key.clone(),
                    payload: published.payload.clone(),
                })?;
            // The outbox entry shares the transaction: an event that exists is always
            // one that will be published.
            trx.realtime_outbox_entry()
                .create(realtime_outbox_entry::Create {
                    key: event.id.to_string(),
                    claimed_by: None,
                    claimed_until_millis: None,
                    attempts: 0,
                    published: false,
                })?;
            Ok(Ok(()))
        })
    }

    fn read(&self, stream: &str, after: u64, limit: usize) -> PortResult<Vec<Publication>> {
        self.0.read(|trx| {
            trx.realtime_log_event()
                .find_many(
                    FindMany::filter(
                        realtime_log_event::stream()
                            .eq(stream)
                            .and(realtime_log_event::sequence().gt(signed(after)?)),
                    )
                    .order_by(realtime_log_event::sequence().asc())
                    .take(limit.max(1) as u64),
                )?
                .into_iter()
                .map(publication)
                .collect()
        })
    }

    fn bounds(&self, stream: &str) -> PortResult<(Option<u64>, Option<u64>)> {
        self.0.read(|trx| {
            let edge = |order| {
                trx.realtime_log_event()
                    .find_first(
                        FindMany::filter(realtime_log_event::stream().eq(stream)).order_by(order),
                    )
                    .map(|row| row.and_then(|row| u64::try_from(row.sequence).ok()))
            };
            Ok((
                edge(realtime_log_event::sequence().desc())?,
                edge(realtime_log_event::sequence().asc())?,
            ))
        })
    }

    fn purge_expired(&self, now_millis: i64) -> PortResult<u64> {
        // Durable events are never purged by time; the others go by their class. An
        // event's outbox entry goes with it.
        self.0.decide(|trx| {
            let expired = Where::Or(vec![
                realtime_log_event::retention()
                    .eq("transient")
                    .and(realtime_log_event::at_millis().lt(now_millis - TRANSIENT_MILLIS)),
                realtime_log_event::retention()
                    .eq("standard")
                    .and(realtime_log_event::at_millis().lt(now_millis - STANDARD_MILLIS)),
            ]);
            let ids: Vec<String> = trx
                .realtime_log_event()
                .find_where(expired.clone())?
                .into_iter()
                .map(|row| row.key)
                .collect();
            if ids.is_empty() {
                return Ok(Ok(0));
            }
            trx.realtime_outbox_entry().delete_many(Some(
                realtime_outbox_entry::key().is_in(ids.iter().cloned()),
            ))?;
            Ok(Ok(trx.realtime_log_event().delete_many(Some(expired))?))
        })
    }
}

impl CheckpointStore for StorageRealtime {
    fn checkpoint(&self, consumer: &str, stream: &str) -> PortResult<Option<Checkpoint>> {
        self.0.read(|trx| {
            trx.realtime_checkpoint()
                .find_unique(realtime_checkpoint::by_key(checkpoint_key(
                    consumer, stream,
                )))?
                .map(|row| {
                    Ok(Checkpoint {
                        consumer: row.consumer,
                        stream: row.stream,
                        sequence: u64::try_from(row.sequence)
                            .map_err(|error| StorageError::invalid(error.to_string()))?,
                        at_millis: row.at_millis,
                    })
                })
                .transpose()
        })
    }

    fn record(&self, checkpoint: &Checkpoint) -> PortResult<()> {
        let key = checkpoint_key(&checkpoint.consumer, &checkpoint.stream);
        self.0.decide(|trx| {
            let sequence = signed(checkpoint.sequence)?;
            match trx
                .realtime_checkpoint()
                .find_unique(realtime_checkpoint::by_key(key.clone()))?
            {
                // Moving backwards is refused: the consumer is told rather than
                // silently replaying.
                Some(current) if current.sequence > sequence => {
                    return Ok(Err(PortError::Conflict));
                }
                Some(_) => {
                    trx.realtime_checkpoint().update(
                        realtime_checkpoint::by_key(key.clone()),
                        realtime_checkpoint::update()
                            .sequence(sequence)
                            .at_millis(checkpoint.at_millis),
                    )?;
                }
                None => {
                    trx.realtime_checkpoint()
                        .create(realtime_checkpoint::Create {
                            key: key.clone(),
                            consumer: checkpoint.consumer.clone(),
                            stream: checkpoint.stream.clone(),
                            sequence,
                            at_millis: checkpoint.at_millis,
                        })?;
                }
            }
            Ok(Ok(()))
        })
    }
}

fn checkpoint_key(consumer: &str, stream: &str) -> String {
    format!("{consumer}::{stream}")
}

impl Outbox for StorageRealtime {
    fn claim(&self, worker: &str, limit: usize, until_millis: i64) -> PortResult<Claim> {
        let claimed = self.0.decide(|trx| {
            let claimable = realtime_outbox_entry::published()
                .eq(false)
                .and(realtime_outbox_entry::attempts().lt(MAX_ATTEMPTS))
                .and(Where::Or(vec![
                    realtime_outbox_entry::claimed_until_millis().is_null(),
                    realtime_outbox_entry::claimed_until_millis().lt(until_millis),
                ]));
            let entries = trx.realtime_outbox_entry().find_many(
                FindMany::filter(claimable)
                    .order_by(realtime_outbox_entry::key().asc())
                    .take(limit.max(1) as u64),
            )?;
            let mut ids = Vec::with_capacity(entries.len());
            for entry in entries {
                // Rewritten with the revision just read: a worker that claimed it
                // first makes this commit conflict, and the claim is decided again.
                trx.realtime_outbox_entry().update(
                    realtime_outbox_entry::by_key(entry.key.clone()),
                    realtime_outbox_entry::update()
                        .claimed_by(Some(worker.to_owned()))
                        .claimed_until_millis(Some(until_millis))
                        .attempts(entry.attempts + 1),
                )?;
                ids.push(entry.key);
            }
            Ok(Ok(ids))
        })?;
        let events = self.0.read(|trx| events(trx, &claimed))?;
        Ok(Claim {
            events,
            until_millis,
        })
    }

    fn complete(&self, worker: &str, ids: &[Uuid]) -> PortResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.0.decide(|trx| {
            // Scoped to the claiming worker: another worker completing this claim
            // would mark published something it never published.
            for id in ids {
                let key = id.to_string();
                let Some(entry) = trx
                    .realtime_outbox_entry()
                    .find_unique(realtime_outbox_entry::by_key(key.clone()))?
                else {
                    return Ok(Err(PortError::Conflict));
                };
                if entry.published || entry.claimed_by.as_deref() != Some(worker) {
                    return Ok(Err(PortError::Conflict));
                }
                trx.realtime_outbox_entry().update(
                    realtime_outbox_entry::by_key(key),
                    realtime_outbox_entry::update()
                        .published(true)
                        .claimed_by(None)
                        .claimed_until_millis(None),
                )?;
            }
            Ok(Ok(()))
        })
    }

    fn release(&self, worker: &str, ids: &[Uuid]) -> PortResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.0.decide(|trx| {
            for id in ids {
                let key = id.to_string();
                if let Some(entry) = trx
                    .realtime_outbox_entry()
                    .find_unique(realtime_outbox_entry::by_key(key.clone()))?
                    && entry.claimed_by.as_deref() == Some(worker)
                {
                    trx.realtime_outbox_entry().update(
                        realtime_outbox_entry::by_key(key),
                        realtime_outbox_entry::update()
                            .claimed_by(None)
                            .claimed_until_millis(None),
                    )?;
                }
            }
            Ok(Ok(()))
        })
    }

    fn dead_letters(&self, limit: usize) -> PortResult<Vec<Publication>> {
        self.0.read(|trx| {
            let ids: Vec<String> = trx
                .realtime_outbox_entry()
                .find_where(
                    realtime_outbox_entry::published()
                        .eq(false)
                        .and(realtime_outbox_entry::attempts().gte(MAX_ATTEMPTS)),
                )?
                .into_iter()
                .map(|entry| entry.key)
                .collect();
            let mut dead = events(trx, &ids)?;
            dead.sort_by_key(|publication| publication.event.at_millis);
            dead.truncate(limit.max(1));
            Ok(dead)
        })
    }
}
