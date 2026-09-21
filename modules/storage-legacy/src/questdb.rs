//! The legacy QuestDB time-series store (PostgreSQL wire) behind the provider seam.
//! The node's log methods and the migration export share these row types; QuestDB
//! client types never leave this module.

use super::*;
use aseman_domain::signal_tags::{LogQuery, validate_tag};
use postgres::tls::NoTls;
use r2d2_postgres::PostgresConnectionManager;
use std::time::Duration;

/// Hard cap on rows one signal read may return (the log is unbounded).
pub const MAX_SIGNAL_ROWS: i64 = 500;

const CREATE_STORAGE: &str = "create table if not exists storage(id text, store_id text, user_id text, data text, tags text, time bigint, edited boolean);";
const CREATE_STORAGE_FRESH: &str = "create table storage(id text, store_id text, user_id text, data text, tags text, time bigint, edited boolean);";

/// Pooled QuestDB client for the `storage` (signals) and `buildlogs` tables.
pub struct QuestDbTimeSeries {
    pool: r2d2::Pool<PostgresConnectionManager<NoTls>>,
}

fn unavailable(context: &str, error: impl std::fmt::Display) -> LegacyMigrationError {
    LegacyMigrationError::Storage(format!("{context}: {error}"))
}

impl QuestDbTimeSeries {
    /// Connect and prepare both tables, exactly as the legacy node did at startup:
    /// retry until `storage` is creatable, repair a ghost table, and refuse to start
    /// if signal tags could not be persisted.
    pub fn connect(port: u16) -> LegacyMigrationResult<Self> {
        let config = format!(
            "host=localhost port={port} user=admin password=quest dbname=qdb sslmode=disable"
        );
        let manager = PostgresConnectionManager::new(
            config.parse().map_err(|error| unavailable("parse QuestDB config", error))?,
            NoTls,
        );
        // Signal persistence runs inside a state modification, so a sick QuestDB
        // must fail one request quickly instead of holding the state lock.
        let pool = r2d2::Pool::builder()
            .connection_timeout(Duration::from_secs(3))
            .build(manager)
            .map_err(|error| unavailable("QuestDB pool", error))?;
        loop {
            let Ok(mut client) = pool.get() else {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            };
            match client.execute(CREATE_STORAGE, &[]) {
                Ok(_) => break,
                Err(error) => {
                    eprintln!("create storage table: {error}");
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
        let mut client = pool.get().map_err(|error| unavailable("pool get", error))?;
        // A node created before signal tags existed lacks the column.
        let _ = client.execute("alter table storage add column if not exists tags text;", &[]);
        // QuestDB may report success for a ghost table that SELECT cannot see.
        if client.query("select tags from storage limit 1", &[]).is_err() {
            let _ = client.execute("drop table if exists storage;", &[]);
            client
                .execute(CREATE_STORAGE_FRESH, &[])
                .map_err(|error| unavailable("recreate storage table", error))?;
        }
        client.query("select tags from storage limit 1", &[]).map_err(|error| {
            unavailable(
                "storage table has no usable `tags` column; signal persistence would \
                 silently drop every message, so the node will not start",
                error,
            )
        })?;
        client
            .execute(
                "create table if not exists buildlogs(id text, build_id text, machine_id text, vm_id text, log_type text, data text, time bigint);",
                &[],
            )
            .map_err(|error| unavailable("create buildlogs", error))?;
        for column in ["vm_id text", "log_type text", "time bigint"] {
            let _ = client.execute(
                &format!("alter table buildlogs add column if not exists {column};"),
                &[],
            );
        }
        drop(client);
        Ok(Self { pool })
    }

    fn client(&self) -> LegacyMigrationResult<r2d2::PooledConnection<PostgresConnectionManager<NoTls>>> {
        self.pool.get().map_err(|error| unavailable("signal log unavailable", error))
    }

    /// Insert one signal; a failed write is an error (the row is the message).
    pub fn insert_signal(&self, row: &LegacySignalRow) -> LegacyMigrationResult<()> {
        self.client()?
            .execute(
                "INSERT INTO storage (id, store_id, user_id, data, tags, time, edited) VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[&row.id, &row.store_id, &row.user_id, &row.data, &row.encoded_tags, &row.time_millis, &row.edited],
            )
            .map(|_| ())
            .map_err(|error| unavailable("signal log write failed", error))
    }

    /// Best-effort legacy edit (it only matches rows already marked edited).
    pub fn update_signal(&self, store_id: &str, signal_id: &str, data: &str) {
        if let Ok(mut client) = self.client() {
            let _ = client.execute(
                "update storage set data = $1 where store_id = $2 and id = $3 and edited = $4",
                &[&data, &store_id, &signal_id, &true],
            );
        }
    }

    fn signal_rows(store_id: &str, rows: Vec<postgres::Row>) -> Vec<LegacySignalRow> {
        rows.into_iter()
            .map(|row| LegacySignalRow {
                id: row.get(0),
                store_id: store_id.to_owned(),
                user_id: row.get(1),
                data: row.get(2),
                encoded_tags: row.get::<_, Option<String>>(3).unwrap_or_default(),
                time_millis: row.get(4),
                edited: row.get(5),
            })
            .collect()
    }

    /// A store's signals, newest first, filtered by validated tags and time bounds.
    pub fn read_signals(&self, store_id: &str, query: &LogQuery) -> LegacyMigrationResult<Vec<LegacySignalRow>> {
        let mut client = self.client()?;
        let count = if query.count <= 0 || query.count > MAX_SIGNAL_ROWS { MAX_SIGNAL_ROWS } else { query.count };
        // Tag predicates are inlined, which is injection-safe only because every
        // tag is validated first (no quote, separator, or wildcard).
        let invalid = |error: aseman_domain::signal_tags::SignalTagError| {
            LegacyMigrationError::Invalid(error.to_string())
        };
        let mut clauses = vec!["store_id = $1".to_owned()];
        for tag in &query.tags_all {
            validate_tag(tag).map_err(invalid)?;
            clauses.push(format!("tags LIKE '%|{tag}|%'"));
        }
        if !query.tags_any.is_empty() {
            let mut anys = Vec::with_capacity(query.tags_any.len());
            for tag in &query.tags_any {
                validate_tag(tag).map_err(invalid)?;
                anys.push(format!("tags LIKE '%|{tag}|%'"));
            }
            clauses.push(format!("({})", anys.join(" OR ")));
        }
        if query.before_time > 0 {
            clauses.push(format!("time < {}", query.before_time));
        }
        if query.after_time > 0 {
            clauses.push(format!("time > {}", query.after_time));
        }
        // QuestDB takes no bound parameter in LIMIT; `count` is a validated i64.
        let sql = format!(
            "SELECT id, user_id, data, tags, time, edited FROM storage WHERE {} order by time desc limit {count}",
            clauses.join(" AND ")
        );
        let rows = client
            .query(&sql, &[&store_id])
            .map_err(|error| unavailable("signal log read failed", error))?;
        Ok(Self::signal_rows(store_id, rows))
    }

    /// Specific signals by ID (best effort, like legacy: failures read as empty).
    pub fn pick_signals(&self, store_id: &str, ids: &[String]) -> Vec<LegacySignalRow> {
        if ids.is_empty() {
            return Vec::new();
        }
        let Ok(mut client) = self.client() else {
            return Vec::new();
        };
        // QuestDB's PG wire lacks universal array parameters; quotes are escaped.
        let quoted = ids
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, user_id, data, tags, time, edited FROM storage WHERE store_id = $1 and id in ({quoted})"
        );
        client
            .query(&sql, &[&store_id])
            .map(|rows| Self::signal_rows(store_id, rows))
            .unwrap_or_default()
    }

    /// Best-effort VM log insert (a streaming VM must never block on its log).
    pub fn insert_build_log(&self, row: &LegacyBuildLogRow) {
        if let Ok(mut client) = self.client() {
            let _ = client.execute(
                "INSERT INTO buildlogs (id, build_id, machine_id, vm_id, log_type, data, time) VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[&row.id, &row.build_id, &row.machine_id, &row.vm_id, &row.log_type, &row.data, &row.time_millis],
            );
        }
    }

    /// A VM's logs, newest first, using QuestDB's `LIMIT lo, hi` range form.
    pub fn read_build_logs(&self, vm_id: &str, log_type: &str, offset: i64, count: i64) -> Vec<LegacyBuildLogRow> {
        let count = if count <= 0 { 100 } else { count };
        let lo = offset.max(0);
        let hi = lo + count;
        let Ok(mut client) = self.client() else {
            return Vec::new();
        };
        let rows = if log_type.is_empty() {
            client.query(
                &format!("SELECT id, build_id, machine_id, vm_id, log_type, data, time FROM buildlogs WHERE vm_id = $1 ORDER BY time DESC LIMIT {lo}, {hi}"),
                &[&vm_id],
            )
        } else {
            client.query(
                &format!("SELECT id, build_id, machine_id, vm_id, log_type, data, time FROM buildlogs WHERE vm_id = $1 AND log_type = $2 ORDER BY time DESC LIMIT {lo}, {hi}"),
                &[&vm_id, &log_type],
            )
        };
        rows.map(|rows| {
            rows.into_iter()
                .map(|row| LegacyBuildLogRow {
                    id: row.get(0),
                    build_id: row.get(1),
                    machine_id: row.get(2),
                    vm_id: row.get(3),
                    log_type: row.get(4),
                    data: row.get(5),
                    time_millis: row.get(6),
                })
                .collect()
        })
        .unwrap_or_default()
    }
}
