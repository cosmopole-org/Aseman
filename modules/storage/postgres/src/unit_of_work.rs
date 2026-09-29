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
use aseman_capsule::{CapsuleStore, CapsuleStoreError, CapsuleStoreResult};
use aseman_config::CapsuleLayout;
use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery};
use aseman_postgres::{Connection, Database, Pool, pool};
use std::sync::Mutex;

/// Opens units of work on a bounded connection pool.
pub struct PostgresUnitOfWorkFactory {
    pool: Pool,
    generation: Option<u64>,
    layout: CapsuleLayout,
}

impl PostgresUnitOfWorkFactory {
    /// A factory over `connection_uri`, fencing writes at `generation` when given.
    pub fn connect(
        connection_uri: &str,
        max_connections: u32,
        generation: Option<u64>,
    ) -> StorageResult<Self> {
        let database =
            Database::parse(connection_uri).map_err(PostgresStorageError::Unavailable)?;
        let pool = pool(&database, max_connections)
            .map_err(|error| PostgresStorageError::Unavailable(error.to_string()))?;
        // Units write in the layout the database was last migrated to (ADR 0034).
        let layout = pool
            .get()
            .map_err(|error| PostgresStorageError::Unavailable(error.to_string()))
            .and_then(|mut connection| crate::layout::recorded_layout(&mut *connection))?;
        Ok(Self {
            pool,
            generation,
            layout,
        })
    }

    /// The layout this factory's units write.
    #[must_use]
    pub fn layout(&self) -> CapsuleLayout {
        self.layout
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
            layout: self.layout,
        })
    }
}

/// One open transaction. Dropping it without [`Self::commit`] rolls it back.
pub struct PostgresUnitOfWork {
    connection: Mutex<Option<Connection>>,
    generation: Option<u64>,
    layout: CapsuleLayout,
}

impl PostgresUnitOfWork {
    pub(crate) fn with_client<T>(
        &self,
        operation: impl FnOnce(&mut postgres::Client) -> StorageResult<T>,
    ) -> StorageResult<T> {
        self.with_connection(operation)
    }

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

    /// Phase one of a cross-shard commit: make the unit's writes durable and
    /// prepared under `gid`, keeping the connection to finish phase two.
    pub fn prepare(self, gid: &str) -> StorageResult<PreparedUnit> {
        if !gid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(PostgresStorageError::Invalid(
                "prepared transaction ids are alphanumeric".to_owned(),
            ));
        }
        let mut guard = self.connection.lock().map_err(|_| {
            PostgresStorageError::Unavailable("unit of work lock poisoned".to_owned())
        })?;
        let mut connection = guard.take().ok_or_else(|| {
            PostgresStorageError::Unavailable("unit of work is finished".to_owned())
        })?;
        connection
            .batch_execute(&format!("PREPARE TRANSACTION '{gid}'"))
            .map_err(map_postgres_error)?;
        Ok(PreparedUnit {
            connection,
            gid: gid.to_owned(),
        })
    }

    /// A query that also returns each row's sort-column values, for merging the
    /// results of several shards in the provider's own order.
    pub(crate) fn query_keyed(
        &self,
        query: &CapsuleQuery,
    ) -> StorageResult<Vec<(Vec<crate::SortValue>, CapsuleEnvelope)>> {
        self.with_connection(|client| crate::query_keyed_on(client, query))
    }

    pub(crate) fn finish(&self, statement: &str) -> StorageResult<()> {
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
            prepared.push(prepare_write(capsule, *expected_revision, self.layout)?);
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

/// A unit prepared by [`PostgresUnitOfWork::prepare`], awaiting phase two.
pub struct PreparedUnit {
    connection: Connection,
    gid: String,
}

impl PreparedUnit {
    pub fn gid(&self) -> &str {
        &self.gid
    }

    /// Phase two: make the prepared writes visible.
    pub fn commit(mut self) -> StorageResult<()> {
        let statement = format!("COMMIT PREPARED '{}'", self.gid);
        self.connection
            .batch_execute(&statement)
            .map_err(map_postgres_error)
    }

    /// Abandon the prepared writes.
    pub fn rollback(mut self) -> StorageResult<()> {
        let statement = format!("ROLLBACK PREPARED '{}'", self.gid);
        self.connection
            .batch_execute(&statement)
            .map_err(map_postgres_error)
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
