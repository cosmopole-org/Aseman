//! One PostgreSQL transaction per node action (ADR 0026).
//!
//! Legacy commits an action's writes to every family in one RocksDB batch. Once core
//! families are served by PostgreSQL, a [`PostgresUnitOfWork`] keeps that atomicity:
//! it opens a transaction, runs every capsule read and write of the action on the same
//! connection (so the action reads its own writes), and commits or rolls back once.
//! Each write runs under a savepoint, so a refused write (a conflict or a fence) leaves
//! the rest of the transaction usable. Every write is fenced at the unit's binding
//! generation (A309).

use crate::{
    PostgresStorageError, SCHEMA, StorageResult, get_on, map_postgres_error, prepare_write,
    query_on, write_prepared,
};
use aseman_capsule_repositories::{CapsuleStore, CapsuleStoreError, CapsuleStoreResult};
use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery};
use postgres::NoTls;
use r2d2::{Pool, PooledConnection};
use r2d2_postgres::PostgresConnectionManager;
use std::sync::Mutex;

type Manager = PostgresConnectionManager<NoTls>;

/// Opens units of work on a bounded connection pool.
pub struct PostgresUnitOfWorkFactory {
    pool: Pool<Manager>,
    generation: Option<u64>,
}

impl PostgresUnitOfWorkFactory {
    /// A factory over `connection_uri`, fencing writes at `generation` when given.
    pub fn connect(
        connection_uri: &str,
        max_connections: u32,
        generation: Option<u64>,
    ) -> StorageResult<Self> {
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
        Ok(Self { pool, generation })
    }

    /// Begin a unit of work on its own connection.
    pub fn begin(&self) -> StorageResult<PostgresUnitOfWork> {
        let mut connection = self
            .pool
            .get()
            .map_err(|error| PostgresStorageError::Unavailable(error.to_string()))?;
        connection
            .batch_execute("BEGIN")
            .map_err(map_postgres_error)?;
        Ok(PostgresUnitOfWork {
            connection: Mutex::new(Some(connection)),
            generation: self.generation,
        })
    }
}

/// One open transaction. Dropping it without [`Self::commit`] rolls it back.
pub struct PostgresUnitOfWork {
    connection: Mutex<Option<PooledConnection<Manager>>>,
    generation: Option<u64>,
}

impl PostgresUnitOfWork {
    fn with_connection<T>(
        &self,
        operation: impl FnOnce(&mut postgres::Client) -> StorageResult<T>,
    ) -> StorageResult<T> {
        let mut guard = self.connection.lock().map_err(|_| {
            PostgresStorageError::Unavailable("unit of work lock poisoned".to_owned())
        })?;
        let connection = guard.as_mut().ok_or_else(|| {
            PostgresStorageError::Unavailable("unit of work is finished".to_owned())
        })?;
        operation(connection)
    }

    /// Commit every write of the unit.
    pub fn commit(self) -> StorageResult<()> {
        self.finish("COMMIT")
    }

    /// Discard every write of the unit.
    pub fn rollback(self) -> StorageResult<()> {
        self.finish("ROLLBACK")
    }

    fn finish(&self, statement: &str) -> StorageResult<()> {
        let mut guard = self.connection.lock().map_err(|_| {
            PostgresStorageError::Unavailable("unit of work lock poisoned".to_owned())
        })?;
        match guard.take() {
            Some(mut connection) => connection
                .batch_execute(statement)
                .map_err(map_postgres_error),
            None => Ok(()),
        }
    }

    fn write_all(&self, writes: &[(CapsuleEnvelope, Option<u64>)]) -> StorageResult<()> {
        let mut prepared = Vec::with_capacity(writes.len());
        for (capsule, expected_revision) in writes {
            prepared.push(prepare_write(capsule, *expected_revision)?);
        }
        let generation = self.generation;
        self.with_connection(|client| {
            client
                .batch_execute("SAVEPOINT aseman_write")
                .map_err(map_postgres_error)?;
            let written = (|| {
                if let Some(generation) = generation {
                    let minimum: i64 = client
                        .query_one(
                            &format!(
                                "SELECT min_generation FROM {SCHEMA}.migration_fence FOR SHARE"
                            ),
                            &[],
                        )
                        .map_err(map_postgres_error)?
                        .get(0);
                    if i64::try_from(generation).map_or(true, |generation| generation < minimum) {
                        return Err(PostgresStorageError::Conflict);
                    }
                }
                for write in &prepared {
                    write_prepared(client, write)?;
                }
                Ok(())
            })();
            let settle = if written.is_ok() {
                "RELEASE SAVEPOINT aseman_write"
            } else {
                "ROLLBACK TO SAVEPOINT aseman_write"
            };
            client.batch_execute(settle).map_err(map_postgres_error)?;
            written
        })
    }
}

impl Drop for PostgresUnitOfWork {
    fn drop(&mut self) {
        // An unfinished unit never leaks its writes into the pool's next user.
        let _ = self.finish("ROLLBACK");
    }
}

fn store_error(error: PostgresStorageError) -> CapsuleStoreError {
    match error {
        PostgresStorageError::Conflict => CapsuleStoreError::Conflict,
        other => CapsuleStoreError::Failed(other.to_string()),
    }
}

impl CapsuleStore for PostgresUnitOfWork {
    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> CapsuleStoreResult<Option<CapsuleEnvelope>> {
        self.with_connection(|client| get_on(client, kind, id))
            .map_err(store_error)
    }

    fn put(
        &self,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> CapsuleStoreResult<()> {
        self.write_all(&[(capsule.clone(), expected_revision)])
            .map_err(store_error)
    }

    fn put_all(&self, writes: &[(CapsuleEnvelope, Option<u64>)]) -> CapsuleStoreResult<()> {
        self.write_all(writes).map_err(store_error)
    }

    fn query(&self, query: &CapsuleQuery) -> CapsuleStoreResult<Vec<CapsuleEnvelope>> {
        self.with_connection(|client| query_on(client, query))
            .map_err(store_error)
    }
}
