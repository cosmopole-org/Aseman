//! Durable realtime on PostgreSQL (A707, ADR 0014).
//!
//! The log here is authoritative, not a cache of a broker. An event is inserted in the
//! same transaction as the application change that caused it, so "it happened" and
//! "it was announced" cannot disagree.

use aseman_domain::Uuid;
use aseman_domain::realtime::{Checkpoint, Event, RetentionClass};
use aseman_ports::realtime::{CheckpointStore, Claim, EventLog, Outbox, Publication};
use aseman_ports::{PortError, PortResult};
use postgres::NoTls;
use r2d2::{Pool, PooledConnection};
use r2d2_postgres::PostgresConnectionManager;

/// Idempotent schema migration owned by the durable realtime provider.
pub const REALTIME_MIGRATION: &str = include_str!("../migrations/0001_realtime.sql");

type Connection = PooledConnection<PostgresConnectionManager<NoTls>>;

/// How many times an event may fail to publish before an operator must look at it.
const MAX_ATTEMPTS: i32 = 8;

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

/// The realtime provider.
pub struct PostgresRealtime {
    pool: Pool<PostgresConnectionManager<NoTls>>,
}

impl PostgresRealtime {
    #[must_use]
    pub fn new(pool: Pool<PostgresConnectionManager<NoTls>>) -> Self {
        Self { pool }
    }

    /// Connect to `url` with a pool of at most `max_size` connections.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the URL is invalid or the pool cannot be built.
    pub fn connect(url: &str, max_size: u32) -> PortResult<Self> {
        let config = url
            .parse::<postgres::Config>()
            .map_err(|_| PortError::Unavailable("invalid realtime database URL"))?;
        let pool = Pool::builder()
            .max_size(max_size.max(1))
            .build(PostgresConnectionManager::new(config, NoTls))
            .map_err(|_| PortError::Unavailable("realtime database"))?;
        Ok(Self { pool })
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
            .map_err(|_| PortError::Unavailable("realtime database"))?;
        Ok(Self { pool })
    }

    /// Create the realtime tables. Idempotent.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub fn migrate(&self) -> PortResult<()> {
        self.connection()?
            .batch_execute(REALTIME_MIGRATION)
            .map_err(db)
    }

    fn connection(&self) -> PortResult<Connection> {
        self.pool
            .get()
            .map_err(|_| PortError::Unavailable("postgres"))
    }

    fn row_to_publication(row: &postgres::Row) -> PortResult<Publication> {
        let sequence: i64 = row.get("sequence");
        Ok(Publication {
            event: Event {
                id: row.get("id"),
                stream: row.get("stream"),
                creature_id: row.get("creature_id"),
                kind: row.get("kind"),
                producer: row.get("producer"),
                sequence: u64::try_from(sequence).map_err(failed)?,
                at_millis: row.get("at_millis"),
                payload_digest: row.get("payload_digest"),
                retention: retention_class(row.get("retention")),
                version: row.get("version"),
                idempotency_key: row.get("idempotency_key"),
            },
            payload: row.get("payload"),
        })
    }
}

impl EventLog for PostgresRealtime {
    fn append(&self, publication: &Publication) -> PortResult<()> {
        let event = &publication.event;
        let mut connection = self.connection()?;
        let mut transaction = connection.transaction().map_err(db)?;
        // The unique key on (stream, sequence) refuses a repeat; this check refuses a
        // gap. Both matter: a gap makes a consumer wait forever.
        let last: Option<i64> = transaction
            .query_one(
                "SELECT MAX(sequence) AS last FROM aseman_core.realtime_event WHERE stream = $1",
                &[&event.stream],
            )
            .map_err(db)?
            .get("last");
        let expected = last.map_or(1, |last| last + 1);
        if i64::try_from(event.sequence).map_err(failed)? != expected {
            return Err(PortError::Conflict);
        }
        transaction
            .execute(
                "INSERT INTO aseman_core.realtime_event \
                   (id, stream, sequence, creature_id, kind, producer, at_millis, \
                    payload_digest, retention, version, idempotency_key, payload) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                &[
                    &event.id,
                    &event.stream,
                    &i64::try_from(event.sequence).map_err(failed)?,
                    &event.creature_id,
                    &event.kind,
                    &event.producer,
                    &event.at_millis,
                    &event.payload_digest,
                    &retention_name(event.retention),
                    &event.version,
                    &event.idempotency_key,
                    &publication.payload,
                ],
            )
            .map_err(db)?;
        // The outbox row is written in the same transaction: an event that exists is
        // always one that will be published.
        transaction
            .execute(
                "INSERT INTO aseman_core.realtime_outbox (event_id) VALUES ($1)",
                &[&event.id],
            )
            .map_err(db)?;
        transaction.commit().map_err(db)?;
        Ok(())
    }

    fn read(&self, stream: &str, after: u64, limit: usize) -> PortResult<Vec<Publication>> {
        let mut connection = self.connection()?;
        let rows = connection
            .query(
                "SELECT * FROM aseman_core.realtime_event \
                 WHERE stream = $1 AND sequence > $2 ORDER BY sequence LIMIT $3",
                &[
                    &stream,
                    &i64::try_from(after).map_err(failed)?,
                    &i64::try_from(limit.max(1)).map_err(failed)?,
                ],
            )
            .map_err(db)?;
        rows.iter().map(Self::row_to_publication).collect()
    }

    fn bounds(&self, stream: &str) -> PortResult<(Option<u64>, Option<u64>)> {
        let mut connection = self.connection()?;
        let row = connection
            .query_one(
                "SELECT MAX(sequence) AS last, MIN(sequence) AS oldest \
                 FROM aseman_core.realtime_event WHERE stream = $1",
                &[&stream],
            )
            .map_err(db)?;
        let last: Option<i64> = row.get("last");
        let oldest: Option<i64> = row.get("oldest");
        Ok((
            last.map(|value| u64::try_from(value).unwrap_or_default()),
            oldest.map(|value| u64::try_from(value).unwrap_or_default()),
        ))
    }

    fn purge_expired(&self, now_millis: i64) -> PortResult<u64> {
        let mut connection = self.connection()?;
        // Durable events are never purged by time; the others go by their class.
        let removed = connection
            .execute(
                "DELETE FROM aseman_core.realtime_event \
                 WHERE (retention = 'transient' AND at_millis < $1) \
                    OR (retention = 'standard' AND at_millis < $2)",
                &[
                    &(now_millis - 60 * 60 * 1000),
                    &(now_millis - 7 * 24 * 60 * 60 * 1000),
                ],
            )
            .map_err(db)?;
        Ok(removed)
    }
}

impl CheckpointStore for PostgresRealtime {
    fn checkpoint(&self, consumer: &str, stream: &str) -> PortResult<Option<Checkpoint>> {
        let mut connection = self.connection()?;
        connection
            .query_opt(
                "SELECT sequence, at_millis FROM aseman_core.realtime_checkpoint \
                 WHERE consumer = $1 AND stream = $2",
                &[&consumer, &stream],
            )
            .map_err(db)?
            .map(|row| {
                let sequence: i64 = row.get("sequence");
                Ok(Checkpoint {
                    consumer: consumer.to_owned(),
                    stream: stream.to_owned(),
                    sequence: u64::try_from(sequence).map_err(failed)?,
                    at_millis: row.get("at_millis"),
                })
            })
            .transpose()
    }

    fn record(&self, checkpoint: &Checkpoint) -> PortResult<()> {
        let mut connection = self.connection()?;
        let sequence = i64::try_from(checkpoint.sequence).map_err(failed)?;
        // The `WHERE` is the guard: a checkpoint that would move backwards updates no
        // row, and the consumer is told rather than silently replaying.
        let updated = connection
            .execute(
                "INSERT INTO aseman_core.realtime_checkpoint \
                   (consumer, stream, sequence, at_millis) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (consumer, stream) DO UPDATE \
                   SET sequence = EXCLUDED.sequence, at_millis = EXCLUDED.at_millis \
                 WHERE aseman_core.realtime_checkpoint.sequence <= EXCLUDED.sequence",
                &[
                    &checkpoint.consumer,
                    &checkpoint.stream,
                    &sequence,
                    &checkpoint.at_millis,
                ],
            )
            .map_err(db)?;
        if updated == 0 {
            return Err(PortError::Conflict);
        }
        Ok(())
    }
}

impl Outbox for PostgresRealtime {
    fn claim(&self, worker: &str, limit: usize, until_millis: i64) -> PortResult<Claim> {
        let mut connection = self.connection()?;
        // `SKIP LOCKED` is why two workers never block each other. Only one holds the
        // publication lease (ADR 0013), so they never race either — but a worker that
        // died mid-batch must not hold its rows forever, which is what the claim
        // deadline is for.
        let rows = connection
            .query(
                "WITH claimed AS ( \
                   SELECT event_id FROM aseman_core.realtime_outbox \
                   WHERE NOT published \
                     AND attempts < $4 \
                     AND (claimed_until_millis IS NULL OR claimed_until_millis < $3) \
                   ORDER BY event_id LIMIT $2 FOR UPDATE SKIP LOCKED \
                 ) \
                 UPDATE aseman_core.realtime_outbox AS outbox \
                 SET claimed_by = $1, claimed_until_millis = $5, attempts = outbox.attempts + 1 \
                 FROM claimed WHERE outbox.event_id = claimed.event_id \
                 RETURNING outbox.event_id",
                &[
                    &worker,
                    &i64::try_from(limit.max(1)).map_err(failed)?,
                    &until_millis,
                    &MAX_ATTEMPTS,
                    &until_millis,
                ],
            )
            .map_err(db)?;
        let ids: Vec<Uuid> = rows.iter().map(|row| row.get("event_id")).collect();
        if ids.is_empty() {
            return Ok(Claim {
                events: Vec::new(),
                until_millis,
            });
        }
        let events = connection
            .query(
                "SELECT * FROM aseman_core.realtime_event WHERE id = ANY($1) ORDER BY sequence",
                &[&ids],
            )
            .map_err(db)?;
        Ok(Claim {
            events: events
                .iter()
                .map(Self::row_to_publication)
                .collect::<PortResult<_>>()?,
            until_millis,
        })
    }

    fn complete(&self, worker: &str, ids: &[Uuid]) -> PortResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection()?;
        // Scoped to the claiming worker: another worker completing this claim would
        // mark something published that it never published.
        let updated = connection
            .execute(
                "UPDATE aseman_core.realtime_outbox SET published = true, claimed_by = NULL, \
                   claimed_until_millis = NULL \
                 WHERE event_id = ANY($1) AND claimed_by = $2 AND NOT published",
                &[&ids, &worker],
            )
            .map_err(db)?;
        if updated as usize != ids.len() {
            return Err(PortError::Conflict);
        }
        Ok(())
    }

    fn release(&self, worker: &str, ids: &[Uuid]) -> PortResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection()?;
        connection
            .execute(
                "UPDATE aseman_core.realtime_outbox \
                 SET claimed_by = NULL, claimed_until_millis = NULL \
                 WHERE event_id = ANY($1) AND claimed_by = $2",
                &[&ids, &worker],
            )
            .map_err(db)?;
        Ok(())
    }

    fn dead_letters(&self, limit: usize) -> PortResult<Vec<Publication>> {
        let mut connection = self.connection()?;
        let rows = connection
            .query(
                "SELECT event.* FROM aseman_core.realtime_event AS event \
                 JOIN aseman_core.realtime_outbox AS outbox ON outbox.event_id = event.id \
                 WHERE NOT outbox.published AND outbox.attempts >= $1 \
                 ORDER BY event.at_millis LIMIT $2",
                &[&MAX_ATTEMPTS, &i64::try_from(limit.max(1)).map_err(failed)?],
            )
            .map_err(db)?;
        rows.iter().map(Self::row_to_publication).collect()
    }
}
