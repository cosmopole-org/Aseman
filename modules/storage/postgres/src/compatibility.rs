//! PostgreSQL implementation of the node's transitional transaction primitives.
//!
//! This module is an adapter boundary, not a new application API.  It maps the old
//! object/index/link/document/raw needs to separate relational tables.  In particular,
//! relation groups are rows with `(relation_type, scope, member)` columns and documents
//! are JSONB; neither is flattened into an opaque key/value namespace.

use std::sync::Mutex;

use postgres::NoTls;
use r2d2::{Pool, PooledConnection};
use r2d2_postgres::PostgresConnectionManager;
use serde_json::Value;

use crate::{PostgresStorageError, StorageResult, map_postgres_error};

type Manager = PostgresConnectionManager<NoTls>;

/// Opens isolated compatibility transactions on a bounded connection pool.
pub struct PostgresCompatibilityTransactionFactory {
    pool: Pool<Manager>,
}

impl PostgresCompatibilityTransactionFactory {
    /// Connect to the provider database.
    pub fn connect(connection_uri: &str, max_connections: u32) -> StorageResult<Self> {
        let manager = PostgresConnectionManager::new(
            connection_uri.parse().map_err(|error: postgres::Error| {
                PostgresStorageError::Unavailable(error.to_string())
            })?,
            NoTls,
        );
        let pool = Pool::builder()
            .max_size(max_connections.max(1))
            .build(manager)
            .map_err(|error| PostgresStorageError::Unavailable(error.to_string()))?;
        Ok(Self { pool })
    }

    /// Begin one serializable transaction.  A read-only compatibility handle still
    /// permits read-your-writes internally, but always rolls back at `commit`.
    pub fn begin(&self, readonly: bool) -> StorageResult<PostgresCompatibilityTransaction> {
        let mut connection = self
            .pool
            .get()
            .map_err(|error| PostgresStorageError::Unavailable(error.to_string()))?;
        connection
            .batch_execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
            .map_err(map_postgres_error)?;
        Ok(PostgresCompatibilityTransaction {
            connection: Mutex::new(Some(connection)),
            readonly,
        })
    }
}

/// One PostgreSQL transaction backing an `ITrx` compatibility adapter.
pub struct PostgresCompatibilityTransaction {
    connection: Mutex<Option<PooledConnection<Manager>>>,
    readonly: bool,
}

impl PostgresCompatibilityTransaction {
    fn with_connection<T>(
        &self,
        operation: impl FnOnce(&mut postgres::Client) -> StorageResult<T>,
    ) -> StorageResult<T> {
        let mut guard = self.connection.lock().map_err(|_| {
            PostgresStorageError::Unavailable("compatibility transaction lock poisoned".into())
        })?;
        let connection = guard.as_mut().ok_or_else(|| {
            PostgresStorageError::Unavailable("compatibility transaction is finished".into())
        })?;
        operation(connection)
    }

    fn finish(&self, statement: &str) -> StorageResult<()> {
        let mut guard = self.connection.lock().map_err(|_| {
            PostgresStorageError::Unavailable("compatibility transaction lock poisoned".into())
        })?;
        match guard.take() {
            Some(mut connection) => connection
                .batch_execute(statement)
                .map_err(map_postgres_error),
            None => Ok(()),
        }
    }

    /// Commit all structured compatibility mutations atomically.  Read-only handles
    /// roll back, matching the old transaction's non-persisting read mode.
    pub fn commit(&self) -> StorageResult<()> {
        self.finish(if self.readonly { "ROLLBACK" } else { "COMMIT" })
    }

    /// Roll back all mutations.  Repeated finalization is harmless.
    pub fn rollback(&self) -> StorageResult<()> {
        self.finish("ROLLBACK")
    }

    /// Read the compatibility byte representation of one exact physical key.
    pub fn value(&self, legacy_key: &str) -> StorageResult<Option<Vec<u8>>> {
        self.with_connection(|client| {
            client
                .query_opt(
                    "SELECT value FROM (\
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
                     ) AS compatibility_values WHERE legacy_key = $1 LIMIT 1",
                    &[&legacy_key],
                )
                .map_err(map_postgres_error)
                .map(|row| row.map(|row| row.get(0)))
        })
    }

    /// Every compatibility key with the prefix, in byte-compatible lexical order.
    pub fn keys_with_prefix(&self, prefix: &str) -> StorageResult<Vec<String>> {
        let pattern = prefix_pattern(prefix);
        self.with_connection(|client| {
            client
                .query(
                    "SELECT legacy_key FROM (\
                       SELECT legacy_key FROM aseman_compat.object_columns \
                       UNION ALL SELECT legacy_key FROM aseman_compat.secondary_indexes \
                       UNION ALL SELECT legacy_key FROM aseman_compat.relations \
                       UNION ALL SELECT legacy_key FROM aseman_compat.documents \
                       UNION ALL SELECT legacy_key FROM aseman_compat.opaque_values\
                     ) AS compatibility_keys \
                     WHERE legacy_key LIKE $1 ESCAPE E'\\\\' \
                     ORDER BY legacy_key COLLATE \"C\"",
                    &[&pattern],
                )
                .map_err(map_postgres_error)
                .map(|rows| rows.into_iter().map(|row| row.get(0)).collect())
        })
    }

    /// Delete an exact physical key from its structured table.
    pub fn delete_key(&self, legacy_key: &str) -> StorageResult<()> {
        self.with_connection(|client| {
            for statement in [
                "DELETE FROM aseman_compat.object_columns WHERE legacy_key = $1",
                "DELETE FROM aseman_compat.secondary_indexes WHERE legacy_key = $1",
                "DELETE FROM aseman_compat.relations WHERE legacy_key = $1",
                "DELETE FROM aseman_compat.documents WHERE legacy_key = $1",
                "DELETE FROM aseman_compat.opaque_values WHERE legacy_key = $1",
            ] {
                client
                    .execute(statement, &[&legacy_key])
                    .map_err(map_postgres_error)?;
            }
            Ok(())
        })
    }

    pub fn put_object_column(
        &self,
        kind: &str,
        object_id: &str,
        column: &str,
        value: &[u8],
    ) -> StorageResult<()> {
        self.with_connection(|client| {
            client
                .execute(
                    "INSERT INTO aseman_compat.object_columns \
                       (object_kind, object_id, column_name, value) VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (object_kind, object_id, column_name) \
                     DO UPDATE SET value = EXCLUDED.value",
                    &[&kind, &object_id, &column, &value],
                )
                .map_err(map_postgres_error)?;
            Ok(())
        })
    }

    /// Read one object without key reconstruction or per-column round trips.
    pub fn object_columns(
        &self,
        kind: &str,
        object_id: &str,
    ) -> StorageResult<Vec<(String, Vec<u8>)>> {
        self.with_connection(|client| {
            client
                .query(
                    "SELECT column_name, value FROM aseman_compat.object_columns \
                     WHERE object_kind = $1 AND object_id = $2 \
                     ORDER BY column_name COLLATE \"C\"",
                    &[&kind, &object_id],
                )
                .map_err(map_postgres_error)
                .map(|rows| {
                    rows.into_iter()
                        .map(|row| (row.get(0), row.get(1)))
                        .collect()
                })
        })
    }

    /// Stream a kind's rows through its structured covering order for list operations.
    pub fn object_kind_columns(&self, kind: &str) -> StorageResult<Vec<(String, String, Vec<u8>)>> {
        self.with_connection(|client| {
            client
                .query(
                    "SELECT object_id, column_name, value \
                     FROM aseman_compat.object_columns WHERE object_kind = $1 \
                     ORDER BY object_id COLLATE \"C\", column_name COLLATE \"C\"",
                    &[&kind],
                )
                .map_err(map_postgres_error)
                .map(|rows| {
                    rows.into_iter()
                        .map(|row| (row.get(0), row.get(1), row.get(2)))
                        .collect()
                })
        })
    }

    pub fn put_secondary_index(
        &self,
        kind: &str,
        from_column: &str,
        to_column: &str,
        from_value: &str,
        to_value: &[u8],
    ) -> StorageResult<()> {
        self.with_connection(|client| {
            client
                .execute(
                    "INSERT INTO aseman_compat.secondary_indexes \
                       (object_kind, from_column, to_column, from_value, to_value) \
                     VALUES ($1, $2, $3, $4, $5) \
                     ON CONFLICT (object_kind, from_column, to_column, from_value) \
                     DO UPDATE SET to_value = EXCLUDED.to_value",
                    &[&kind, &from_column, &to_column, &from_value, &to_value],
                )
                .map_err(map_postgres_error)?;
            Ok(())
        })
    }

    /// Scan one declared secondary-index family in its native table.
    pub fn secondary_index_entries(
        &self,
        kind: &str,
        from_column: &str,
        to_column: &str,
    ) -> StorageResult<Vec<(String, Vec<u8>)>> {
        self.with_connection(|client| {
            client
                .query(
                    "SELECT from_value, to_value FROM aseman_compat.secondary_indexes \
                     WHERE object_kind = $1 AND from_column = $2 AND to_column = $3 \
                     ORDER BY from_value COLLATE \"C\"",
                    &[&kind, &from_column, &to_column],
                )
                .map_err(map_postgres_error)
                .map(|rows| {
                    rows.into_iter()
                        .map(|row| (row.get(0), row.get(1)))
                        .collect()
                })
        })
    }

    /// Upsert one normalized relation row.
    pub fn put_relation(
        &self,
        relation_type: &str,
        scope: &str,
        member: &str,
        logical_key: &str,
        value: &str,
    ) -> StorageResult<()> {
        self.with_connection(|client| {
            client
                .execute(
                    "INSERT INTO aseman_compat.relations \
                       (relation_type, scope, member, logical_key, value) \
                     VALUES ($1, $2, $3, $4, $5) \
                     ON CONFLICT (logical_key) DO UPDATE SET \
                       relation_type = EXCLUDED.relation_type, scope = EXCLUDED.scope, \
                       member = EXCLUDED.member, value = EXCLUDED.value",
                    &[&relation_type, &scope, &member, &logical_key, &value],
                )
                .map_err(map_postgres_error)?;
            Ok(())
        })
    }

    pub fn put_document(&self, key: &str, path: &str, value: &Value) -> StorageResult<()> {
        self.with_connection(|client| {
            client
                .execute(
                    "INSERT INTO aseman_compat.documents (document_key, path, document) \
                     VALUES ($1, $2, $3) ON CONFLICT (document_key, path) \
                     DO UPDATE SET document = EXCLUDED.document",
                    &[&key, &path, &value],
                )
                .map_err(map_postgres_error)?;
            Ok(())
        })
    }

    pub fn document(&self, key: &str, path: &str) -> StorageResult<Option<Value>> {
        self.with_connection(|client| {
            client
                .query_opt(
                    "SELECT document FROM aseman_compat.documents \
                     WHERE document_key = $1 AND path = $2",
                    &[&key, &path],
                )
                .map_err(map_postgres_error)
                .map(|row| row.map(|row| row.get(0)))
        })
    }

    pub fn delete_document_tree(&self, key: &str, path: &str) -> StorageResult<()> {
        let descendant = prefix_pattern(&format!("{path}."));
        self.with_connection(|client| {
            client
                .execute(
                    "DELETE FROM aseman_compat.documents WHERE document_key = $1 \
                     AND (path = $2 OR path LIKE $3 ESCAPE E'\\\\')",
                    &[&key, &path, &descendant],
                )
                .map_err(map_postgres_error)?;
            Ok(())
        })
    }

    pub fn put_opaque(&self, key: &str, value: &[u8]) -> StorageResult<()> {
        self.with_connection(|client| {
            client
                .execute(
                    "INSERT INTO aseman_compat.opaque_values (legacy_key, value) VALUES ($1, $2) \
                     ON CONFLICT (legacy_key) DO UPDATE SET value = EXCLUDED.value",
                    &[&key, &value],
                )
                .map_err(map_postgres_error)?;
            Ok(())
        })
    }
}

impl Drop for PostgresCompatibilityTransaction {
    fn drop(&mut self) {
        let _ = self.finish("ROLLBACK");
    }
}

fn prefix_pattern(prefix: &str) -> String {
    let mut pattern = String::with_capacity(prefix.len() + 1);
    for character in prefix.chars() {
        if matches!(character, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_patterns_escape_sql_wildcards() {
        assert_eq!(prefix_pattern(r"group%_\"), r"group\%\_\\%");
    }
}
