//! PostgreSQL per-creature database provisioning, bounded pools, and typed schema DDL.

use aseman_contracts::guest::{
    GuestBindingStatus, GuestColumnDefinition, GuestDatabaseBinding, GuestDefaultValue,
    GuestDeleteAction, GuestFieldType, GuestIndexDefinition, GuestSchemaCommand,
    GuestSchemaMutation, GuestTableDefinition, MAX_GUEST_COLUMNS, MAX_GUEST_INDEXES,
    MAX_GUEST_TABLES,
};
use postgres::{Client, Config, NoTls, Transaction};
use r2d2::Pool;
use r2d2_postgres::PostgresConnectionManager;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;
use thiserror::Error;

mod kv;
pub use kv::PostgresGuestKv;

const PROVIDER_ID: &str = "postgres-guest-v1";
const GUEST_SCHEMA: &str = "aseman_guest";
const GUARD_SCHEMA: &str = "aseman_guard";

#[derive(Debug, Error)]
pub enum GuestPostgresError {
    #[error("invalid guest database request: {0}")]
    Invalid(String),
    #[error("guest database binding is not active")]
    Inactive,
    #[error("guest database pool capacity is exhausted")]
    PoolCapacity,
    #[error("guest schema catalog revision conflict")]
    SchemaConflict,
    #[error("guest database authorization state was contaminated")]
    Contaminated,
    #[error("PostgreSQL guest operation failed: {0}")]
    Database(String),
}

pub type GuestPostgresResult<T> = Result<T, GuestPostgresError>;

#[derive(Clone, Debug)]
pub struct ProvisionedGuestDatabase {
    binding: GuestDatabaseBinding,
}

impl ProvisionedGuestDatabase {
    #[must_use]
    pub fn binding(&self) -> &GuestDatabaseBinding {
        &self.binding
    }

    fn validate_derived(&self) -> GuestPostgresResult<()> {
        self.binding
            .validate()
            .map_err(|error| GuestPostgresError::Invalid(error.to_string()))?;
        let (database, role, _) = derived_names(self.binding.creature_id, self.binding.generation)?;
        if self.binding.provider_id != PROVIDER_ID
            || self.binding.database_name != database
            || self.binding.role_name != role
        {
            return Err(GuestPostgresError::Invalid(
                "binding names were not derived by the provider".to_owned(),
            ));
        }
        Ok(())
    }
}

pub struct PostgresGuestProvisioner {
    admin: Config,
    proxy_role: String,
}

impl PostgresGuestProvisioner {
    pub fn new(admin_connection_uri: &str, proxy_role: &str) -> GuestPostgresResult<Self> {
        validate_identifier(proxy_role)?;
        let admin = Config::from_str(admin_connection_uri)
            .map_err(|error| GuestPostgresError::Invalid(error.to_string()))?;
        Ok(Self {
            admin,
            proxy_role: proxy_role.to_owned(),
        })
    }

    pub fn provision(
        &self,
        creature_id: [u8; 16],
        generation: u64,
    ) -> GuestPostgresResult<ProvisionedGuestDatabase> {
        let (database, role, lock_key) = derived_names(creature_id, generation)?;
        let mut admin = self.connect_admin()?;
        let proxy_login: Option<bool> = admin
            .query_opt(
                "SELECT rolcanlogin FROM pg_roles WHERE rolname = $1",
                &[&self.proxy_role],
            )
            .map_err(database_error)?
            .map(|row| row.get(0));
        if proxy_login != Some(true) {
            return Err(GuestPostgresError::Invalid(
                "trusted guest proxy role is absent or cannot login".to_owned(),
            ));
        }
        admin
            .query_one("SELECT pg_advisory_lock($1)", &[&lock_key])
            .map_err(database_error)?;
        let result = self.provision_locked(&mut admin, &database, &role);
        let _ = admin.query_one("SELECT pg_advisory_unlock($1)", &[&lock_key]);
        result?;
        Ok(ProvisionedGuestDatabase {
            binding: GuestDatabaseBinding {
                creature_id,
                provider_id: PROVIDER_ID.to_owned(),
                database_name: database,
                role_name: role,
                generation,
                schema_catalog_revision: 0,
                status: GuestBindingStatus::Disabled,
            },
        })
    }

    pub fn enable(
        &self,
        database: &ProvisionedGuestDatabase,
    ) -> GuestPostgresResult<ProvisionedGuestDatabase> {
        database.validate_derived()?;
        let mut admin = self.connect_admin()?;
        admin
            .batch_execute(&format!(
                "GRANT CONNECT ON DATABASE {} TO {}, {};",
                quoted(&database.binding.database_name),
                quoted(&self.proxy_role),
                quoted(&database.binding.role_name)
            ))
            .map_err(database_error)?;
        let mut active = database.clone();
        active.binding.status = GuestBindingStatus::Active;
        Ok(active)
    }

    pub fn disable(
        &self,
        database: &ProvisionedGuestDatabase,
    ) -> GuestPostgresResult<ProvisionedGuestDatabase> {
        database.validate_derived()?;
        let mut admin = self.connect_admin()?;
        admin
            .batch_execute(&format!(
                "REVOKE CONNECT ON DATABASE {} FROM {}, {};",
                quoted(&database.binding.database_name),
                quoted(&self.proxy_role),
                quoted(&database.binding.role_name)
            ))
            .map_err(database_error)?;
        admin
            .query(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                 WHERE datname = $1 AND pid <> pg_backend_pid()",
                &[&database.binding.database_name],
            )
            .map_err(database_error)?;
        let mut disabled = database.clone();
        disabled.binding.status = GuestBindingStatus::Disabled;
        Ok(disabled)
    }

    fn provision_locked(
        &self,
        admin: &mut Client,
        database: &str,
        role: &str,
    ) -> GuestPostgresResult<()> {
        if admin
            .query_opt("SELECT 1 FROM pg_roles WHERE rolname = $1", &[&role])
            .map_err(database_error)?
            .is_none()
        {
            admin
                .batch_execute(&format!(
                    "CREATE ROLE {} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT \
                     NOREPLICATION NOBYPASSRLS;",
                    quoted(role)
                ))
                .map_err(database_error)?;
        }
        admin
            .batch_execute(&format!(
                "ALTER ROLE {} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT \
                 NOREPLICATION NOBYPASSRLS; ALTER ROLE {} NOINHERIT; GRANT {} TO {};",
                quoted(role),
                quoted(&self.proxy_role),
                quoted(role),
                quoted(&self.proxy_role)
            ))
            .map_err(database_error)?;
        if admin
            .query_opt("SELECT 1 FROM pg_database WHERE datname = $1", &[&database])
            .map_err(database_error)?
            .is_none()
        {
            admin
                .batch_execute(&format!(
                    "CREATE DATABASE {} TEMPLATE template0 ENCODING 'UTF8';",
                    quoted(database)
                ))
                .map_err(database_error)?;
        }
        admin
            .batch_execute(&format!(
                "REVOKE ALL ON DATABASE {} FROM PUBLIC; \
                 REVOKE CONNECT, TEMPORARY ON DATABASE {} FROM {}, {};",
                quoted(database),
                quoted(database),
                quoted(&self.proxy_role),
                quoted(role)
            ))
            .map_err(database_error)?;

        let mut guest_config = self.admin.clone();
        guest_config.dbname(database);
        let mut guest = guest_config.connect(NoTls).map_err(database_error)?;
        guest
            .batch_execute(&format!(
                "REVOKE CREATE ON SCHEMA public FROM PUBLIC; \
                 REVOKE ALL ON SCHEMA public FROM {}; \
                 CREATE SCHEMA IF NOT EXISTS {GUARD_SCHEMA}; \
                 REVOKE ALL ON SCHEMA {GUARD_SCHEMA} FROM PUBLIC, {}; \
                 CREATE TABLE IF NOT EXISTS {GUARD_SCHEMA}.schema_catalog (\
                   singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton), \
                   revision BIGINT NOT NULL CHECK (revision >= 0)\
                 ); \
                 CREATE TABLE IF NOT EXISTS {GUARD_SCHEMA}.schema_changes (\
                   revision BIGINT PRIMARY KEY CHECK (revision > 0), \
                   mutation_json BYTEA NOT NULL, \
                   applied_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()\
                 ); \
                 INSERT INTO {GUARD_SCHEMA}.schema_catalog(singleton, revision) \
                   VALUES (TRUE, 0) ON CONFLICT (singleton) DO NOTHING; \
                 REVOKE ALL ON ALL TABLES IN SCHEMA {GUARD_SCHEMA} FROM PUBLIC, {}; \
                 GRANT USAGE ON SCHEMA {GUARD_SCHEMA} TO {}; \
                 GRANT SELECT, UPDATE ON {GUARD_SCHEMA}.schema_catalog TO {}; \
                 GRANT SELECT, INSERT ON {GUARD_SCHEMA}.schema_changes TO {}; \
                 CREATE SCHEMA IF NOT EXISTS {GUEST_SCHEMA} AUTHORIZATION {}; \
                 ALTER SCHEMA {GUEST_SCHEMA} OWNER TO {}; \
                 REVOKE ALL ON SCHEMA {GUEST_SCHEMA} FROM PUBLIC; \
                 GRANT USAGE, CREATE ON SCHEMA {GUEST_SCHEMA} TO {}; \
                 ALTER ROLE {} IN DATABASE {} SET search_path TO {GUEST_SCHEMA}, pg_catalog;",
                quoted(role),
                quoted(role),
                quoted(role),
                quoted(&self.proxy_role),
                quoted(&self.proxy_role),
                quoted(&self.proxy_role),
                quoted(role),
                quoted(role),
                quoted(role),
                quoted(role),
                quoted(database)
            ))
            .map_err(database_error)
    }

    fn connect_admin(&self) -> GuestPostgresResult<Client> {
        self.admin.clone().connect(NoTls).map_err(database_error)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PoolKey {
    provider_id: String,
    database_name: String,
    role_name: String,
    generation: u64,
}

type GuestPool = Pool<PostgresConnectionManager<NoTls>>;

pub struct GuestPoolRouter {
    proxy: Config,
    proxy_role: String,
    max_pools: usize,
    max_pool_size: u32,
    pools: Mutex<BTreeMap<PoolKey, GuestPool>>,
}

impl GuestPoolRouter {
    pub fn new(
        proxy_connection_uri: &str,
        proxy_role: &str,
        max_pools: usize,
        max_pool_size: u32,
    ) -> GuestPostgresResult<Self> {
        let proxy = Config::from_str(proxy_connection_uri)
            .map_err(|error| GuestPostgresError::Invalid(error.to_string()))?;
        Self::from_config(proxy, proxy_role, max_pools, max_pool_size)
    }

    pub fn from_config(
        proxy: Config,
        proxy_role: &str,
        max_pools: usize,
        max_pool_size: u32,
    ) -> GuestPostgresResult<Self> {
        validate_identifier(proxy_role)?;
        if max_pools == 0 || max_pool_size == 0 {
            return Err(GuestPostgresError::Invalid(
                "guest pool limits must be positive".to_owned(),
            ));
        }
        Ok(Self {
            proxy,
            proxy_role: proxy_role.to_owned(),
            max_pools,
            max_pool_size,
            pools: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn with_transaction<T>(
        &self,
        binding: &ProvisionedGuestDatabase,
        operation: impl FnOnce(&mut Transaction<'_>) -> GuestPostgresResult<T>,
    ) -> GuestPostgresResult<T> {
        binding.validate_derived()?;
        if binding.binding.status != GuestBindingStatus::Active {
            return Err(GuestPostgresError::Inactive);
        }
        let pool = self.pool(binding)?;
        let mut client = pool.get().map_err(|error| {
            GuestPostgresError::Database(format!("guest pool checkout: {error}"))
        })?;
        reset_and_verify(
            &mut client,
            &self.proxy_role,
            &binding.binding.database_name,
        )?;
        let mut transaction = client.transaction().map_err(database_error)?;
        transaction
            .batch_execute(&format!(
                "SET LOCAL ROLE {}; SET LOCAL search_path TO {GUEST_SCHEMA}, pg_catalog;",
                quoted(&binding.binding.role_name)
            ))
            .map_err(database_error)?;
        verify_transaction_identity(
            &mut transaction,
            &binding.binding.role_name,
            &self.proxy_role,
            &binding.binding.database_name,
        )?;
        let result = operation(&mut transaction);
        let result = match result {
            Ok(value) => match verify_transaction_identity(
                &mut transaction,
                &binding.binding.role_name,
                &self.proxy_role,
                &binding.binding.database_name,
            ) {
                Ok(()) => transaction.commit().map_err(database_error).map(|()| value),
                Err(error) => {
                    let _ = transaction.rollback();
                    Err(error)
                }
            },
            Err(error) => {
                let _ = transaction.rollback();
                Err(error)
            }
        };
        let reset = reset_and_verify(
            &mut client,
            &self.proxy_role,
            &binding.binding.database_name,
        );
        match (result, reset) {
            (_, Err(error)) => Err(error),
            (result, Ok(())) => result,
        }
    }

    pub fn retire(&self, binding: &ProvisionedGuestDatabase) -> GuestPostgresResult<()> {
        binding.validate_derived()?;
        self.pools
            .lock()
            .map_err(|_| GuestPostgresError::Database("pool registry poisoned".to_owned()))?
            .remove(&pool_key(binding));
        Ok(())
    }

    fn with_schema_transaction<T>(
        &self,
        binding: &ProvisionedGuestDatabase,
        expected_revision: u64,
        mutation_json: &[u8],
        operation: impl FnOnce(&mut Transaction<'_>) -> GuestPostgresResult<T>,
    ) -> GuestPostgresResult<T> {
        binding.validate_derived()?;
        if binding.binding.status != GuestBindingStatus::Active {
            return Err(GuestPostgresError::Inactive);
        }
        if binding.binding.schema_catalog_revision != expected_revision {
            return Err(GuestPostgresError::SchemaConflict);
        }
        let expected = i64::try_from(expected_revision).map_err(|_| {
            GuestPostgresError::Invalid("schema revision exceeds PostgreSQL range".to_owned())
        })?;
        let next = expected
            .checked_add(1)
            .ok_or_else(|| GuestPostgresError::Invalid("schema revision overflow".to_owned()))?;
        let pool = self.pool(binding)?;
        let mut client = pool.get().map_err(|error| {
            GuestPostgresError::Database(format!("guest pool checkout: {error}"))
        })?;
        reset_and_verify(
            &mut client,
            &self.proxy_role,
            &binding.binding.database_name,
        )?;
        let mut transaction = client.transaction().map_err(database_error)?;
        let current: i64 = transaction
            .query_one(
                "SELECT revision FROM aseman_guard.schema_catalog WHERE singleton FOR UPDATE",
                &[],
            )
            .map_err(database_error)?
            .get(0);
        let result = if current != expected {
            let _ = transaction.rollback();
            Err(GuestPostgresError::SchemaConflict)
        } else {
            transaction
                .batch_execute(&format!(
                    "SET LOCAL ROLE {}; SET LOCAL search_path TO {GUEST_SCHEMA}, pg_catalog;",
                    quoted(&binding.binding.role_name)
                ))
                .map_err(database_error)?;
            verify_transaction_identity(
                &mut transaction,
                &binding.binding.role_name,
                &self.proxy_role,
                &binding.binding.database_name,
            )?;
            match operation(&mut transaction) {
                Ok(value) => {
                    verify_transaction_identity(
                        &mut transaction,
                        &binding.binding.role_name,
                        &self.proxy_role,
                        &binding.binding.database_name,
                    )?;
                    transaction
                        .batch_execute("RESET ROLE; SET LOCAL search_path TO pg_catalog;")
                        .map_err(database_error)?;
                    verify_transaction_identity(
                        &mut transaction,
                        &self.proxy_role,
                        &self.proxy_role,
                        &binding.binding.database_name,
                    )?;
                    transaction
                        .execute(
                            "INSERT INTO aseman_guard.schema_changes(revision, mutation_json) VALUES ($1, $2)",
                            &[&next, &mutation_json],
                        )
                        .map_err(database_error)?;
                    let changed = transaction
                        .execute(
                            "UPDATE aseman_guard.schema_catalog SET revision = $1 WHERE singleton AND revision = $2",
                            &[&next, &expected],
                        )
                        .map_err(database_error)?;
                    if changed != 1 {
                        let _ = transaction.rollback();
                        Err(GuestPostgresError::SchemaConflict)
                    } else {
                        transaction.commit().map_err(database_error).map(|()| value)
                    }
                }
                Err(error) => {
                    let _ = transaction.rollback();
                    Err(error)
                }
            }
        };
        let reset = reset_and_verify(
            &mut client,
            &self.proxy_role,
            &binding.binding.database_name,
        );
        match (result, reset) {
            (_, Err(error)) => Err(error),
            (result, Ok(())) => result,
        }
    }

    fn pool(&self, binding: &ProvisionedGuestDatabase) -> GuestPostgresResult<GuestPool> {
        let key = pool_key(binding);
        let mut pools = self
            .pools
            .lock()
            .map_err(|_| GuestPostgresError::Database("pool registry poisoned".to_owned()))?;
        if let Some(pool) = pools.get(&key) {
            return Ok(pool.clone());
        }
        if pools.len() >= self.max_pools {
            return Err(GuestPostgresError::PoolCapacity);
        }
        let mut config = self.proxy.clone();
        config.dbname(&binding.binding.database_name);
        let manager = PostgresConnectionManager::new(config, NoTls);
        let pool = Pool::builder()
            .max_size(self.max_pool_size)
            .connection_timeout(Duration::from_secs(3))
            .build(manager)
            .map_err(|error| GuestPostgresError::Database(error.to_string()))?;
        pools.insert(key, pool.clone());
        Ok(pool)
    }
}

#[derive(Clone, Debug)]
pub struct GuestSchemaManager {
    pub max_tables: usize,
    pub max_columns_per_table: usize,
    pub max_indexes_per_table: usize,
}

impl Default for GuestSchemaManager {
    fn default() -> Self {
        Self {
            max_tables: MAX_GUEST_TABLES,
            max_columns_per_table: MAX_GUEST_COLUMNS,
            max_indexes_per_table: MAX_GUEST_INDEXES,
        }
    }
}

impl GuestSchemaManager {
    pub fn apply(
        &self,
        pools: &GuestPoolRouter,
        binding: &ProvisionedGuestDatabase,
        command: &GuestSchemaCommand,
    ) -> GuestPostgresResult<ProvisionedGuestDatabase> {
        command
            .validate()
            .map_err(|error| GuestPostgresError::Invalid(error.to_string()))?;
        let mutation_json = serde_json::to_vec(&command.mutation)
            .map_err(|error| GuestPostgresError::Invalid(error.to_string()))?;
        pools.with_schema_transaction(
            binding,
            command.expected_catalog_revision,
            &mutation_json,
            |transaction| match &command.mutation {
                GuestSchemaMutation::CreateTable { definition } => {
                    self.create_table(transaction, definition)
                }
                GuestSchemaMutation::AddColumn {
                    table,
                    name,
                    definition,
                } => self.add_column(transaction, table, name, definition),
                GuestSchemaMutation::CreateIndex { table, definition } => {
                    self.create_index(transaction, table, definition)
                }
                GuestSchemaMutation::DropIndex { table, name } => {
                    self.drop_index(transaction, table, name)
                }
                GuestSchemaMutation::DropTable { table, .. } => transaction
                    .batch_execute(&format!(
                        "DROP TABLE {GUEST_SCHEMA}.{} RESTRICT",
                        quoted(table)
                    ))
                    .map_err(database_error),
            },
        )?;
        let mut updated = binding.clone();
        updated.binding.schema_catalog_revision = command
            .expected_catalog_revision
            .checked_add(1)
            .ok_or_else(|| GuestPostgresError::Invalid("schema revision overflow".to_owned()))?;
        Ok(updated)
    }

    fn create_table(
        &self,
        transaction: &mut Transaction<'_>,
        definition: &GuestTableDefinition,
    ) -> GuestPostgresResult<()> {
        let count: i64 = transaction
            .query_one(
                "SELECT count(*) FROM pg_catalog.pg_tables WHERE schemaname = $1",
                &[&GUEST_SCHEMA],
            )
            .map_err(database_error)?
            .get(0);
        if usize::try_from(count).unwrap_or(usize::MAX) >= self.max_tables
            || definition.columns.len() > self.max_columns_per_table
            || definition.indexes.len() > self.max_indexes_per_table
        {
            return Err(GuestPostgresError::Invalid(
                "guest schema quota exceeded".to_owned(),
            ));
        }
        let mut columns = vec![
            "_aseman_id UUID PRIMARY KEY".to_owned(),
            "_aseman_revision BIGINT NOT NULL CHECK (_aseman_revision > 0)".to_owned(),
            "_aseman_created_at_micros BIGINT NOT NULL".to_owned(),
            "_aseman_updated_at_micros BIGINT NOT NULL CHECK (_aseman_updated_at_micros >= _aseman_created_at_micros)".to_owned(),
            "_aseman_integrity BYTEA NOT NULL CHECK (octet_length(_aseman_integrity) = 32)".to_owned(),
            "_aseman_tombstone BOOLEAN NOT NULL DEFAULT FALSE".to_owned(),
        ];
        columns.extend(
            definition
                .columns
                .iter()
                .map(|(name, column)| column_sql(name, column))
                .collect::<GuestPostgresResult<Vec<_>>>()?,
        );
        if !definition.primary_key.is_empty() {
            columns.push(format!(
                "CONSTRAINT {} UNIQUE ({})",
                quoted(&bounded_name(
                    "uq",
                    &definition.name,
                    &definition.primary_key
                )),
                identifier_list(&definition.primary_key)
            ));
        }
        for foreign_key in &definition.foreign_keys {
            let action = match foreign_key.on_delete {
                GuestDeleteAction::Restrict => "RESTRICT",
                GuestDeleteAction::Cascade => "CASCADE",
                GuestDeleteAction::SetNull => "SET NULL",
            };
            columns.push(format!(
                "CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {GUEST_SCHEMA}.{} ({}) ON DELETE {action}",
                quoted(&foreign_key.name),
                identifier_list(&foreign_key.columns),
                quoted(&foreign_key.target_table),
                identifier_list(&foreign_key.target_columns)
            ));
        }
        transaction
            .batch_execute(&format!(
                "CREATE TABLE {GUEST_SCHEMA}.{} ({})",
                quoted(&definition.name),
                columns.join(", ")
            ))
            .map_err(database_error)?;
        for index in &definition.indexes {
            self.create_index(transaction, &definition.name, index)?;
        }
        Ok(())
    }

    fn add_column(
        &self,
        transaction: &mut Transaction<'_>,
        table: &str,
        name: &str,
        definition: &GuestColumnDefinition,
    ) -> GuestPostgresResult<()> {
        let count: i64 = transaction
            .query_one(
                "SELECT count(*) FROM information_schema.columns \
                 WHERE table_schema = $1 AND table_name = $2",
                &[&GUEST_SCHEMA, &table],
            )
            .map_err(database_error)?
            .get(0);
        if usize::try_from(count).unwrap_or(usize::MAX) >= self.max_columns_per_table + 6 {
            return Err(GuestPostgresError::Invalid(
                "guest column quota exceeded".to_owned(),
            ));
        }
        transaction
            .batch_execute(&format!(
                "ALTER TABLE {GUEST_SCHEMA}.{} ADD COLUMN {}",
                quoted(table),
                column_sql(name, definition)?
            ))
            .map_err(database_error)
    }

    fn create_index(
        &self,
        transaction: &mut Transaction<'_>,
        table: &str,
        definition: &GuestIndexDefinition,
    ) -> GuestPostgresResult<()> {
        let count: i64 = transaction
            .query_one(
                "SELECT count(*) FROM pg_catalog.pg_indexes \
                 WHERE schemaname = $1 AND tablename = $2",
                &[&GUEST_SCHEMA, &table],
            )
            .map_err(database_error)?
            .get(0);
        if usize::try_from(count).unwrap_or(usize::MAX) > self.max_indexes_per_table {
            return Err(GuestPostgresError::Invalid(
                "guest index quota exceeded".to_owned(),
            ));
        }
        transaction
            .batch_execute(&format!(
                "CREATE {} INDEX {} ON {GUEST_SCHEMA}.{} ({})",
                if definition.unique { "UNIQUE" } else { "" },
                quoted(&definition.name),
                quoted(table),
                identifier_list(&definition.columns)
            ))
            .map_err(database_error)
    }

    fn drop_index(
        &self,
        transaction: &mut Transaction<'_>,
        table: &str,
        name: &str,
    ) -> GuestPostgresResult<()> {
        let belongs = transaction
            .query_opt(
                "SELECT 1 FROM pg_catalog.pg_indexes \
                 WHERE schemaname = $1 AND tablename = $2 AND indexname = $3",
                &[&GUEST_SCHEMA, &table, &name],
            )
            .map_err(database_error)?
            .is_some();
        if !belongs {
            return Err(GuestPostgresError::Invalid(
                "guest index does not belong to the resolved table".to_owned(),
            ));
        }
        transaction
            .batch_execute(&format!("DROP INDEX {GUEST_SCHEMA}.{}", quoted(name)))
            .map_err(database_error)
    }
}

fn pool_key(binding: &ProvisionedGuestDatabase) -> PoolKey {
    PoolKey {
        provider_id: binding.binding.provider_id.clone(),
        database_name: binding.binding.database_name.clone(),
        role_name: binding.binding.role_name.clone(),
        generation: binding.binding.generation,
    }
}

fn reset_and_verify(
    client: &mut Client,
    proxy_role: &str,
    database: &str,
) -> GuestPostgresResult<()> {
    client
        .batch_execute("RESET ROLE; RESET ALL; DISCARD TEMP;")
        .map_err(database_error)?;
    let row = client
        .query_one(
            "SELECT current_user::text, session_user::text, current_database()::text",
            &[],
        )
        .map_err(database_error)?;
    let current: String = row.get(0);
    let session: String = row.get(1);
    let current_database: String = row.get(2);
    if current != proxy_role || session != proxy_role || current_database != database {
        return Err(GuestPostgresError::Contaminated);
    }
    Ok(())
}

fn verify_transaction_identity(
    transaction: &mut Transaction<'_>,
    role: &str,
    proxy_role: &str,
    database: &str,
) -> GuestPostgresResult<()> {
    let row = transaction
        .query_one(
            "SELECT current_user::text, session_user::text, current_database()::text",
            &[],
        )
        .map_err(database_error)?;
    let current: String = row.get(0);
    let session: String = row.get(1);
    let current_database: String = row.get(2);
    if current != role || session != proxy_role || current_database != database {
        return Err(GuestPostgresError::Contaminated);
    }
    Ok(())
}

fn derived_names(
    creature_id: [u8; 16],
    generation: u64,
) -> GuestPostgresResult<(String, String, i64)> {
    if generation == 0 {
        return Err(GuestPostgresError::Invalid(
            "guest binding generation starts at one".to_owned(),
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(b"ASEMAN-POSTGRES-GUEST-V1\0");
    hasher.update(creature_id);
    hasher.update(generation.to_be_bytes());
    let digest = hasher.finalize();
    let suffix = digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let lock_key = i64::from_be_bytes(
        digest[..8]
            .try_into()
            .map_err(|_| GuestPostgresError::Invalid("guest binding digest length".to_owned()))?,
    );
    Ok((
        format!("aseman_guest_{suffix}"),
        format!("aseman_creature_{suffix}"),
        lock_key,
    ))
}

fn column_sql(name: &str, definition: &GuestColumnDefinition) -> GuestPostgresResult<String> {
    let sql_type = match definition.field_type {
        GuestFieldType::Bool => "BOOLEAN",
        GuestFieldType::Integer | GuestFieldType::TimestampMicros => "BIGINT",
        GuestFieldType::Float => "DOUBLE PRECISION",
        GuestFieldType::Bytes => "BYTEA",
        GuestFieldType::Text => "TEXT",
        GuestFieldType::CapsuleId => "UUID",
    };
    let mut sql = format!("{} {sql_type}", quoted(name));
    if definition.required {
        sql.push_str(" NOT NULL");
    }
    if let Some(default) = &definition.default {
        sql.push_str(" DEFAULT ");
        sql.push_str(&default_sql(&definition.field_type, default)?);
    }
    Ok(sql)
}

fn default_sql(
    field_type: &GuestFieldType,
    default: &GuestDefaultValue,
) -> GuestPostgresResult<String> {
    match (field_type, default) {
        (GuestFieldType::Bool, GuestDefaultValue::Bool(value)) => {
            Ok(if *value { "TRUE" } else { "FALSE" }.to_owned())
        }
        (
            GuestFieldType::Integer | GuestFieldType::TimestampMicros,
            GuestDefaultValue::Integer(value),
        ) => Ok(value.to_string()),
        (GuestFieldType::Float, GuestDefaultValue::Float(value)) => {
            let parsed = value.parse::<f64>().map_err(|_| {
                GuestPostgresError::Invalid("invalid portable float default".to_owned())
            })?;
            if !parsed.is_finite() {
                return Err(GuestPostgresError::Invalid(
                    "non-finite float default".to_owned(),
                ));
            }
            Ok(parsed.to_string())
        }
        (GuestFieldType::Bytes, GuestDefaultValue::Bytes(value)) => {
            Ok(format!("decode('{}', 'hex')", hex_bytes(value)))
        }
        (GuestFieldType::Text, GuestDefaultValue::Text(value)) => {
            Ok(format!("'{}'", value.replace('\'', "''")))
        }
        (GuestFieldType::CapsuleId, GuestDefaultValue::Bytes(value)) if value.len() == 16 => {
            let hex = hex_bytes(value);
            Ok(format!(
                "'{}-{}-{}-{}-{}'::uuid",
                &hex[0..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..32]
            ))
        }
        _ => Err(GuestPostgresError::Invalid(
            "guest default does not match its field type".to_owned(),
        )),
    }
}

fn bounded_name(prefix: &str, table: &str, columns: &[String]) -> String {
    let source = format!("{prefix}_{table}_{}", columns.join("_"));
    if source.len() <= 63 {
        return source;
    }
    let digest = Sha256::digest(source.as_bytes());
    let suffix = digest[..5]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{}_{}", &source[..52], suffix)
}

fn identifier_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| quoted(value))
        .collect::<Vec<_>>()
        .join(", ")
}

fn validate_identifier(value: &str) -> GuestPostgresResult<()> {
    if value.is_empty()
        || value.len() > 63
        || !value.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_lowercase()
            } else {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
            }
        })
    {
        return Err(GuestPostgresError::Invalid(
            "unsafe PostgreSQL identifier".to_owned(),
        ));
    }
    Ok(())
}

fn quoted(value: &str) -> String {
    debug_assert!(validate_identifier(value).is_ok());
    format!("\"{value}\"")
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn database_error(error: postgres::Error) -> GuestPostgresError {
    if let Some(error) = error.as_db_error() {
        GuestPostgresError::Database(format!(
            "{} (SQLSTATE {})",
            error.message(),
            error.code().code()
        ))
    } else {
        GuestPostgresError::Database(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_database_and_role_names_are_stable_safe_and_generation_bound() {
        let first = derived_names([3; 16], 1).unwrap();
        let repeated = derived_names([3; 16], 1).unwrap();
        let next = derived_names([3; 16], 2).unwrap();
        assert_eq!(first, repeated);
        assert_ne!(first.0, next.0);
        assert!(validate_identifier(&first.0).is_ok());
        assert!(validate_identifier(&first.1).is_ok());
    }

    #[test]
    fn defaults_are_rendered_as_values_not_sql_fragments() {
        let text = default_sql(
            &GuestFieldType::Text,
            &GuestDefaultValue::Text("'); DROP DATABASE postgres; --".to_owned()),
        )
        .unwrap();
        assert_eq!(text, "'''); DROP DATABASE postgres; --'");
        assert!(
            default_sql(
                &GuestFieldType::Float,
                &GuestDefaultValue::Float("NaN".to_owned())
            )
            .is_err()
        );
    }
}

/// ADR 0021 reserved, migration-owned table for legacy guest `dbOp` pairs.
pub const LEGACY_KV_TABLE: &str = "_aseman_legacy_kv";
/// The reserved name, pre-quoted. It deliberately bypasses `quoted`, whose user-identifier
/// validation rejects the reserved `_aseman_` prefix that guests may never create.
const LEGACY_KV_TABLE_SQL: &str = "\"_aseman_legacy_kv\"";

/// The reserved legacy KV table (`contracts/capsule/guest/legacy-kv-table.json`), created
/// by the first import or the first gateway write, whichever comes first.
fn legacy_kv_table_ddl() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {GUEST_SCHEMA}.{table} (\
                       _aseman_id UUID PRIMARY KEY, \
                       _aseman_revision BIGINT NOT NULL CHECK (_aseman_revision > 0), \
                       _aseman_created_at_micros BIGINT NOT NULL, \
                       _aseman_updated_at_micros BIGINT NOT NULL, \
                       _aseman_integrity BYTEA NOT NULL CHECK (octet_length(_aseman_integrity) = 32), \
                       _aseman_tombstone BOOLEAN NOT NULL DEFAULT FALSE, \
                       _aseman_capsule_cbor BYTEA NOT NULL, \
                       namespace TEXT NOT NULL CONSTRAINT _aseman_legacy_kv_namespace_check \
                       CHECK (namespace IN ('dbop', 'applet_db', 'json')), \
                       key TEXT NOT NULL, \
                       value TEXT NOT NULL, \
                       UNIQUE (namespace, key))",
        table = LEGACY_KV_TABLE_SQL
    ) + &format!(
        "; DO $$ BEGIN \
           IF NOT EXISTS (SELECT 1 FROM pg_constraint \
             WHERE conname = '_aseman_legacy_kv_namespace_check' \
               AND conrelid = '{GUEST_SCHEMA}.{LEGACY_KV_TABLE_SQL}'::regclass \
               AND pg_get_constraintdef(oid) LIKE '%json%') THEN \
             ALTER TABLE {GUEST_SCHEMA}.{LEGACY_KV_TABLE_SQL} \
               DROP CONSTRAINT IF EXISTS _aseman_legacy_kv_namespace_check, \
               ADD CONSTRAINT _aseman_legacy_kv_namespace_check \
                 CHECK (namespace IN ('dbop', 'applet_db', 'json')); \
           END IF; \
         END $$"
    )
}

/// Outcome of importing legacy guest KV capsules into one creature database.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LegacyKvImport {
    pub inserted: u64,
    pub already_present: u64,
}

impl GuestPoolRouter {
    /// Import `guest.legacy_kv` capsules into the binding's own database (ADR 0021).
    ///
    /// Every capsule must be `GuestData` owned by the binding's creature, so one
    /// creature's pairs can never land in another creature's database. Replays of an
    /// identical capsule are idempotent; a different capsule at the same identity or
    /// `(namespace, key)` is a conflict.
    pub fn import_legacy_kv(
        &self,
        binding: &ProvisionedGuestDatabase,
        capsules: &[aseman_contracts::capsule::CapsuleEnvelope],
    ) -> GuestPostgresResult<LegacyKvImport> {
        use aseman_contracts::capsule::{CapsuleValue, OwnerScope, StorageClass};
        let mut rows = Vec::with_capacity(capsules.len());
        for capsule in capsules {
            capsule
                .verify()
                .map_err(|error| GuestPostgresError::Invalid(error.to_string()))?;
            if capsule.kind.0 != "guest.legacy_kv"
                || capsule.storage_class != StorageClass::GuestData
                || capsule.owner_scope != OwnerScope::Creature(binding.binding.creature_id)
                || capsule.tombstone
            {
                return Err(GuestPostgresError::Invalid(
                    "legacy KV capsule does not belong to this creature database".to_owned(),
                ));
            }
            let Some(CapsuleValue::Object(body)) = &capsule.body else {
                return Err(GuestPostgresError::Invalid(
                    "legacy KV capsule has no body".to_owned(),
                ));
            };
            let text = |name: &str| match body.get(name) {
                Some(CapsuleValue::Text(value)) => Ok(value.clone()),
                _ => Err(GuestPostgresError::Invalid(format!(
                    "legacy KV capsule lacks {name}"
                ))),
            };
            let namespace = text("namespace")?;
            if !matches!(namespace.as_str(), "dbop" | "applet_db" | "json") || body.len() != 3 {
                return Err(GuestPostgresError::Invalid(
                    "legacy KV capsule has an unreviewed shape".to_owned(),
                ));
            }
            let canonical = capsule
                .canonical_bytes()
                .map_err(|error| GuestPostgresError::Invalid(error.to_string()))?;
            rows.push((
                capsule.clone(),
                namespace,
                text("key")?,
                text("value")?,
                canonical,
            ));
        }
        self.with_transaction(binding, |transaction| {
            transaction
                .batch_execute(&legacy_kv_table_ddl())
                .map_err(database_error)?;
            let mut report = LegacyKvImport::default();
            for (capsule, namespace, key, value, canonical) in &rows {
                let id = uuid::Uuid::from_bytes(capsule.id.0);
                let revision = i64::try_from(capsule.revision)
                    .map_err(|_| GuestPostgresError::Invalid("revision overflow".to_owned()))?;
                let inserted = transaction
                    .execute(
                        &format!(
                            "INSERT INTO {GUEST_SCHEMA}.{} (_aseman_id, _aseman_revision, \
                             _aseman_created_at_micros, _aseman_updated_at_micros, _aseman_integrity, \
                             _aseman_capsule_cbor, namespace, key, value) \
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT DO NOTHING",
                            LEGACY_KV_TABLE_SQL
                        ),
                        &[
                            &id,
                            &revision,
                            &capsule.created_at_micros,
                            &capsule.updated_at_micros,
                            &capsule.integrity_hash.bytes,
                            canonical,
                            namespace,
                            key,
                            value,
                        ],
                    )
                    .map_err(database_error)?;
                if inserted == 1 {
                    report.inserted += 1;
                    continue;
                }
                let existing = transaction
                    .query_opt(
                        &format!(
                            "SELECT _aseman_capsule_cbor FROM {GUEST_SCHEMA}.{} \
                             WHERE _aseman_id = $1 AND namespace = $2 AND key = $3",
                            LEGACY_KV_TABLE_SQL
                        ),
                        &[&id, namespace, key],
                    )
                    .map_err(database_error)?
                    .map(|row| row.get::<_, Vec<u8>>(0));
                if existing.as_deref() != Some(canonical.as_slice()) {
                    return Err(GuestPostgresError::Invalid(
                        "legacy KV import conflicts with an existing record".to_owned(),
                    ));
                }
                report.already_present += 1;
            }
            Ok(report)
        })
    }
}
