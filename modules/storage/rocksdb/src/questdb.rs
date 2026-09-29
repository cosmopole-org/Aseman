//! The legacy signal history (ADR 0036): before the storage module, store signals
//! lived in QuestDB's `storage` table (PostgreSQL wire) or, under
//! `ASEMAN_SIGNAL_LOG_PROVIDER=postgres`, in `aseman_legacy_log.signals`. The node
//! keeps them as `realtime.event` models now; `asemanctl storage migrate` reads the
//! old table once through this reader and converts the rows with
//! [`transform_legacy_signal_rows`]. Nothing here writes.

use super::*;
use postgres::{Client, NoTls};

/// The schema holding the signal table when PostgreSQL served it.
pub const POSTGRES_SIGNAL_LOG_SCHEMA: &str = "aseman_legacy_log";

/// Where the legacy signal table is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegacySignalLogSource {
    /// A QuestDB instance on localhost (`storage`).
    QuestDb { port: u16 },
    /// A PostgreSQL database (`aseman_legacy_log.signals`).
    Postgres { url: String },
}

fn unavailable(context: &str, error: impl std::fmt::Display) -> LegacyMigrationError {
    LegacyMigrationError::Storage(format!("{context}: {error}"))
}

/// Every legacy signal row of `source`, by store then time; empty when the table was
/// never created.
///
/// # Errors
///
/// An unreachable source or an unreadable table.
pub fn read_legacy_signals(
    source: &LegacySignalLogSource,
) -> LegacyMigrationResult<Vec<LegacySignalRow>> {
    let (connection, table, exists) = match source {
        LegacySignalLogSource::QuestDb { port } => (
            format!(
                "host=localhost port={port} user=admin password=quest dbname=qdb sslmode=disable connect_timeout=5"
            ),
            "storage",
            "SELECT count(*) > 0 FROM tables() WHERE table_name = 'storage'",
        ),
        LegacySignalLogSource::Postgres { url } => (
            url.clone(),
            "aseman_legacy_log.signals",
            "SELECT to_regclass('aseman_legacy_log.signals') IS NOT NULL",
        ),
    };
    let mut client = Client::connect(&connection, NoTls)
        .map_err(|error| unavailable("legacy signal log", error))?;
    let present: bool = client
        .query_one(exists, &[])
        .map_err(|error| unavailable("legacy signal log", error))?
        .get(0);
    if !present {
        return Ok(Vec::new());
    }
    Ok(client
        .query(
            &format!(
                "SELECT id, store_id, user_id, data, tags, time, edited FROM {table} \
                 ORDER BY store_id, time"
            ),
            &[],
        )
        .map_err(|error| unavailable("legacy signal log read", error))?
        .iter()
        .map(|row| LegacySignalRow {
            id: row.get(0),
            store_id: row.get(1),
            user_id: row.get(2),
            data: row.get(3),
            encoded_tags: row.get::<_, Option<String>>(4).unwrap_or_default(),
            time_millis: row.get(5),
            edited: row.get(6),
        })
        .collect())
}
