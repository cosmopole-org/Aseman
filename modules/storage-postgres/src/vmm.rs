//! The PostgreSQL VMM service stores (A501, A503) in the `aseman_vmm` schema.
//!
//! Records are stored whole as JSON next to the columns that queries and
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
use postgres::NoTls;
use postgres::types::ToSql;
use r2d2::{Pool, PooledConnection};
use r2d2_postgres::PostgresConnectionManager;
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

pub const VMM_MIGRATION: &str = include_str!("../migrations/vmm/0001_vmm.sql");
pub const VMM_EVENT_TIME_MIGRATION: &str = include_str!("../migrations/vmm/0002_event_time.sql");

type Connection = PooledConnection<PostgresConnectionManager<NoTls>>;

/// The VMM service's stores on PostgreSQL.
pub struct PostgresVmmStore {
    pool: Pool<PostgresConnectionManager<NoTls>>,
}

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

fn encode<T: Serialize>(value: &T) -> PortResult<serde_json::Value> {
    serde_json::to_value(value).map_err(failed)
}

fn decode<T: DeserializeOwned>(value: serde_json::Value) -> PortResult<T> {
    serde_json::from_value(value).map_err(failed)
}

fn state_text<T: Serialize>(state: &T) -> PortResult<String> {
    match encode(state)? {
        serde_json::Value::String(text) => Ok(text),
        other => Err(failed(format!("not a state: {other}"))),
    }
}

fn cursor_uuid(cursor: Option<&str>) -> PortResult<Option<Uuid>> {
    cursor
        .map(|cursor| {
            Uuid::parse_str(cursor).map_err(|_| PortError::Failed("invalid cursor".to_owned()))
        })
        .transpose()
}

fn limit_i64(limit: usize) -> i64 {
    i64::try_from(limit.max(1)).unwrap_or(i64::MAX - 1) + 1
}

fn page_of<T>(mut items: Vec<T>, limit: usize, key: impl Fn(&T) -> String) -> Page<T> {
    let more = items.len() > limit.max(1);
    items.truncate(limit.max(1));
    Page {
        next_cursor: if more { items.last().map(key) } else { None },
        items,
    }
}

impl PostgresVmmStore {
    /// Connect with a pool of at most `max_size` connections.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the URL is invalid or the database cannot be reached.
    pub fn connect(url: &str, max_size: u32) -> PortResult<Self> {
        let config = url
            .parse::<postgres::Config>()
            .map_err(|_| PortError::Unavailable("invalid VMM database URL"))?;
        Self::connect_config(config, max_size)
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
            .map_err(|_| PortError::Unavailable("VMM database"))?;
        Ok(Self { pool })
    }

    fn connection(&self) -> PortResult<Connection> {
        self.pool
            .get()
            .map_err(|_| PortError::Unavailable("VMM database"))
    }

    /// Create or upgrade the schema.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub fn migrate(&self) -> PortResult<()> {
        let mut connection = self.connection()?;
        connection.batch_execute(VMM_MIGRATION).map_err(db)?;
        connection
            .batch_execute(VMM_EVENT_TIME_MIGRATION)
            .map_err(db)
    }

    fn workload_rows(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> PortResult<Vec<WorkloadRecord>> {
        self.connection()?
            .query(sql, params)
            .map_err(db)?
            .into_iter()
            .map(|row| decode(row.get(0)))
            .collect()
    }

    fn operation_rows(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> PortResult<Vec<OperationRecord>> {
        self.connection()?
            .query(sql, params)
            .map_err(db)?
            .into_iter()
            .map(|row| decode(row.get(0)))
            .collect()
    }
}

fn observed_text(record: &WorkloadRecord) -> PortResult<Option<String>> {
    record
        .observed
        .as_ref()
        .map(|observed| state_text(&observed.state))
        .transpose()
}

fn version(record: &WorkloadRecord) -> PortResult<i64> {
    i64::try_from(record.resource_version).map_err(failed)
}

impl VmmWorkloadStore for PostgresVmmStore {
    fn workload(&self, owner: &str, id: WorkloadId) -> PortResult<Option<WorkloadRecord>> {
        Ok(self
            .workload_rows(
                "SELECT record FROM aseman_vmm.workload WHERE id = $1 AND owner = $2",
                &[id.as_uuid(), &owner],
            )?
            .pop())
    }

    fn workloads(
        &self,
        owner: &str,
        filter: &WorkloadFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> PortResult<Page<WorkloadRecord>> {
        let after = cursor_uuid(cursor)?;
        let observed = filter.observed_state.as_ref().map(state_text).transpose()?;
        let rows = self.workload_rows(
            "SELECT record FROM aseman_vmm.workload \
             WHERE owner = $1 AND ($2::uuid IS NULL OR id > $2) \
               AND ($3::uuid IS NULL OR creature_id = $3) \
               AND ($4::text IS NULL OR observed_state = $4) \
             ORDER BY id LIMIT $5",
            &[
                &owner,
                &after,
                &filter.creature_id,
                &observed,
                &limit_i64(limit),
            ],
        )?;
        Ok(page_of(rows, limit, |record| record.id.to_string()))
    }

    fn all_workloads(
        &self,
        cursor: Option<&str>,
        limit: usize,
    ) -> PortResult<Page<WorkloadRecord>> {
        let after = cursor_uuid(cursor)?;
        let rows = self.workload_rows(
            "SELECT record FROM aseman_vmm.workload \
             WHERE ($1::uuid IS NULL OR id > $1) ORDER BY id LIMIT $2",
            &[&after, &limit_i64(limit)],
        )?;
        Ok(page_of(rows, limit, |record| record.id.to_string()))
    }

    fn insert_workload(&self, record: &WorkloadRecord) -> PortResult<()> {
        self.connection()?
            .execute(
                "INSERT INTO aseman_vmm.workload \
                 (id, owner, creature_id, observed_state, resource_version, record) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    record.id.as_uuid(),
                    &record.owner,
                    &record.labels.creature_id,
                    &observed_text(record)?,
                    &version(record)?,
                    &encode(record)?,
                ],
            )
            .map_err(db)?;
        Ok(())
    }

    fn replace_workload(&self, record: &WorkloadRecord, expected: u64) -> PortResult<()> {
        let expected = i64::try_from(expected).map_err(failed)?;
        let mut connection = self.connection()?;
        let updated = connection
            .execute(
                "UPDATE aseman_vmm.workload \
                 SET creature_id = $3, observed_state = $4, resource_version = $5, record = $6 \
                 WHERE id = $1 AND owner = $2 AND resource_version = $7",
                &[
                    record.id.as_uuid(),
                    &record.owner,
                    &record.labels.creature_id,
                    &observed_text(record)?,
                    &version(record)?,
                    &encode(record)?,
                    &expected,
                ],
            )
            .map_err(db)?;
        if updated == 1 {
            return Ok(());
        }
        let exists = connection
            .query_opt(
                "SELECT 1 FROM aseman_vmm.workload WHERE id = $1",
                &[record.id.as_uuid()],
            )
            .map_err(db)?
            .is_some();
        Err(if exists {
            PortError::Conflict
        } else {
            PortError::NotFound
        })
    }
}

impl VmmOperationStore for PostgresVmmStore {
    fn operation(&self, owner: &str, id: OperationId) -> PortResult<Option<OperationRecord>> {
        Ok(self
            .operation_rows(
                "SELECT record FROM aseman_vmm.operation WHERE id = $1 AND owner = $2",
                &[id.as_uuid(), &owner],
            )?
            .pop())
    }

    fn operations(
        &self,
        owner: &str,
        filter: &OperationFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> PortResult<Page<OperationRecord>> {
        let after = cursor_uuid(cursor)?;
        let workload = filter.workload_id.map(|id| *id.as_uuid());
        let state = filter.state.as_ref().map(state_text).transpose()?;
        let rows = self.operation_rows(
            "SELECT record FROM aseman_vmm.operation o \
             WHERE owner = $1 \
               AND ($2::uuid IS NULL OR workload_id = $2) \
               AND ($3::text IS NULL OR state = $3) \
               AND ($4::uuid IS NULL OR (created_at_millis, id) < \
                    (SELECT created_at_millis, id FROM aseman_vmm.operation WHERE id = $4)) \
             ORDER BY created_at_millis DESC, id DESC LIMIT $5",
            &[&owner, &workload, &state, &after, &limit_i64(limit)],
        )?;
        Ok(page_of(rows, limit, |record| record.id.to_string()))
    }

    fn insert_operation(&self, record: &OperationRecord) -> PortResult<()> {
        self.connection()?
            .execute(
                "INSERT INTO aseman_vmm.operation \
                 (id, owner, workload_id, state, created_at_millis, record) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    record.id.as_uuid(),
                    &record.owner,
                    &record.workload_id.map(|id| *id.as_uuid()),
                    &state_text(&record.state)?,
                    &record.created_at_millis,
                    &encode(record)?,
                ],
            )
            .map_err(db)?;
        Ok(())
    }

    fn replace_operation(
        &self,
        record: &OperationRecord,
        expected: OperationState,
    ) -> PortResult<()> {
        let mut connection = self.connection()?;
        let updated = connection
            .execute(
                "UPDATE aseman_vmm.operation SET state = $3, record = $4 \
                 WHERE id = $1 AND owner = $2 AND state = $5",
                &[
                    record.id.as_uuid(),
                    &record.owner,
                    &state_text(&record.state)?,
                    &encode(record)?,
                    &state_text(&expected)?,
                ],
            )
            .map_err(db)?;
        if updated == 1 {
            return Ok(());
        }
        let exists = connection
            .query_opt(
                "SELECT 1 FROM aseman_vmm.operation WHERE id = $1",
                &[record.id.as_uuid()],
            )
            .map_err(db)?
            .is_some();
        Err(if exists {
            PortError::Conflict
        } else {
            PortError::NotFound
        })
    }

    fn unfinished_operations(&self, limit: usize) -> PortResult<Vec<OperationRecord>> {
        self.operation_rows(
            "SELECT record FROM aseman_vmm.operation \
             WHERE state IN ('pending', 'running') \
             ORDER BY created_at_millis, id LIMIT $1",
            &[&(limit_i64(limit) - 1)],
        )
    }
}

impl IdempotencyStore for PostgresVmmStore {
    fn claim(
        &self,
        owner: &str,
        key: &str,
        digest: [u8; 32],
        now_millis: i64,
        claim_ttl_millis: i64,
    ) -> PortResult<IdempotencyClaim> {
        let mut connection = self.connection()?;
        // One statement: a new key inserts, an abandoned claim with the same digest is
        // taken over, and anything else returns no row and is classified below.
        let claimed = connection
            .query_opt(
                "INSERT INTO aseman_vmm.idempotency (owner, key, digest, claimed_at_millis) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (owner, key) DO UPDATE SET claimed_at_millis = EXCLUDED.claimed_at_millis \
                   WHERE aseman_vmm.idempotency.response_status IS NULL \
                     AND aseman_vmm.idempotency.digest = EXCLUDED.digest \
                     AND aseman_vmm.idempotency.claimed_at_millis <= $4 - $5 \
                 RETURNING 1",
                &[&owner, &key, &digest.as_slice(), &now_millis, &claim_ttl_millis],
            )
            .map_err(db)?
            .is_some();
        if claimed {
            return Ok(IdempotencyClaim::Claimed);
        }
        let row = connection
            .query_one(
                "SELECT digest, response_status, response_body, response_content_type, \
                        response_location \
                 FROM aseman_vmm.idempotency WHERE owner = $1 AND key = $2",
                &[&owner, &key],
            )
            .map_err(db)?;
        let stored: Vec<u8> = row.get(0);
        if stored != digest {
            return Ok(IdempotencyClaim::Mismatch);
        }
        let status: Option<i32> = row.get(1);
        Ok(match status {
            None => IdempotencyClaim::InProgress,
            Some(status) => IdempotencyClaim::Completed(ReplayableResponse {
                status: u16::try_from(status).map_err(failed)?,
                body: row.get::<_, Option<Vec<u8>>>(2).unwrap_or_default(),
                content_type: row.get::<_, Option<String>>(3).unwrap_or_default(),
                location: row.get(4),
            }),
        })
    }

    fn complete(&self, owner: &str, key: &str, response: &ReplayableResponse) -> PortResult<()> {
        let updated = self
            .connection()?
            .execute(
                "UPDATE aseman_vmm.idempotency SET response_status = $3, response_body = $4, \
                   response_content_type = $5, response_location = $6 \
                 WHERE owner = $1 AND key = $2",
                &[
                    &owner,
                    &key,
                    &i32::from(response.status),
                    &response.body,
                    &response.content_type,
                    &response.location,
                ],
            )
            .map_err(db)?;
        if updated == 1 {
            Ok(())
        } else {
            Err(PortError::NotFound)
        }
    }

    fn release(&self, owner: &str, key: &str) -> PortResult<()> {
        self.connection()?
            .execute(
                "DELETE FROM aseman_vmm.idempotency \
                 WHERE owner = $1 AND key = $2 AND response_status IS NULL",
                &[&owner, &key],
            )
            .map_err(db)?;
        Ok(())
    }

    fn purge_before(&self, cutoff_millis: i64) -> PortResult<u64> {
        self.connection()?
            .execute(
                "DELETE FROM aseman_vmm.idempotency WHERE claimed_at_millis < $1",
                &[&cutoff_millis],
            )
            .map_err(db)
    }
}

impl VmmEventLog for PostgresVmmStore {
    fn append(&self, event: &WorkloadEventRecord) -> PortResult<u64> {
        let mut connection = self.connection()?;
        let mut transaction = connection.transaction().map_err(db)?;
        let sequence: i64 = transaction
            .query_one(
                "UPDATE aseman_vmm.event_log SET last_sequence = last_sequence + 1 \
                 RETURNING last_sequence",
                &[],
            )
            .map_err(db)?
            .get(0);
        let mut event = event.clone();
        event.sequence = u64::try_from(sequence).map_err(failed)?;
        transaction
            .execute(
                "INSERT INTO aseman_vmm.event (sequence, owner, workload_id, record, at_millis) \
                 VALUES ($1, $2, $3, $4, $5)",
                &[
                    &sequence,
                    &event.owner,
                    event.workload_id.as_uuid(),
                    &encode(&event)?,
                    &event.at_millis,
                ],
            )
            .map_err(db)?;
        transaction.commit().map_err(db)?;
        Ok(event.sequence)
    }

    fn events_after(
        &self,
        owner: &str,
        after: u64,
        workload: Option<WorkloadId>,
        limit: usize,
    ) -> PortResult<EventBatch> {
        let after = i64::try_from(after).map_err(failed)?;
        let mut connection = self.connection()?;
        let truncated: i64 = connection
            .query_one("SELECT truncated_through FROM aseman_vmm.event_log", &[])
            .map_err(db)?
            .get(0);
        if after < truncated {
            return Ok(EventBatch {
                events: Vec::new(),
                resync: true,
            });
        }
        let events = connection
            .query(
                "SELECT record FROM aseman_vmm.event \
                 WHERE owner = $1 AND sequence > $2 AND ($3::uuid IS NULL OR workload_id = $3) \
                 ORDER BY sequence LIMIT $4",
                &[
                    &owner,
                    &after,
                    &workload.map(|id| *id.as_uuid()),
                    &(limit_i64(limit) - 1),
                ],
            )
            .map_err(db)?
            .into_iter()
            .map(|row| decode(row.get(0)))
            .collect::<PortResult<Vec<_>>>()?;
        Ok(EventBatch {
            events,
            resync: false,
        })
    }

    fn truncate_before(&self, cutoff_millis: i64) -> PortResult<u64> {
        let mut connection = self.connection()?;
        let mut transaction = connection.transaction().map_err(db)?;
        // Serialize with appends, so a sequence committed later is never dropped.
        transaction
            .execute("SELECT 1 FROM aseman_vmm.event_log FOR UPDATE", &[])
            .map_err(db)?;
        let dropped = transaction
            .query(
                "DELETE FROM aseman_vmm.event WHERE at_millis < $1 RETURNING sequence",
                &[&cutoff_millis],
            )
            .map_err(db)?;
        if let Some(highest) = dropped.iter().map(|row| row.get::<_, i64>(0)).max() {
            transaction
                .execute(
                    "UPDATE aseman_vmm.event_log \
                     SET truncated_through = GREATEST(truncated_through, $1)",
                    &[&highest],
                )
                .map_err(db)?;
        }
        transaction.commit().map_err(db)?;
        Ok(dropped.len() as u64)
    }
}

#[cfg(test)]
mod tests;
