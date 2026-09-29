//! The node's legacy compatibility schema (`aseman_compat`), read once by the storage
//! migration (ADR 0036). The node no longer writes it: its transitional key/value
//! adapter was replaced by the storage module, and `asemanctl storage migrate`
//! converts what remains and retires the schema.

use aseman_postgres::Database;

use crate::{StorageResult, map_postgres_error};

/// Every legacy physical record the compatibility schema holds, as `(legacy key,
/// value bytes)` in key order: the source of `asemanctl storage migrate` for a
/// PostgreSQL store from before ADR 0036.
pub fn legacy_records(connection_uri: &str) -> StorageResult<Vec<(String, Vec<u8>)>> {
    let mut client = Database::parse(connection_uri)
        .map_err(crate::PostgresStorageError::Unavailable)?
        .connect()
        .map_err(map_postgres_error)?;
    let present: bool = client
        .query_one("SELECT to_regnamespace('aseman_compat') IS NOT NULL", &[])
        .map_err(map_postgres_error)?
        .get(0);
    if !present {
        return Ok(Vec::new());
    }
    Ok(client
        .query(
            "SELECT legacy_key, value FROM (\
               SELECT legacy_key, value FROM aseman_compat.object_columns \
               UNION ALL \
               SELECT legacy_key, to_value AS value FROM aseman_compat.secondary_indexes \
               UNION ALL \
               SELECT legacy_key, convert_to(value, 'UTF8') AS value \
                 FROM aseman_compat.relations \
               UNION ALL \
               SELECT legacy_key, convert_to(document::text, 'UTF8') AS value \
                 FROM aseman_compat.documents \
               UNION ALL \
               SELECT legacy_key, value FROM aseman_compat.opaque_values\
             ) AS compatibility_values ORDER BY legacy_key COLLATE \"C\"",
            &[],
        )
        .map_err(map_postgres_error)?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect())
}
