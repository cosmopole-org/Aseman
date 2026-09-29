//! The PostgreSQL storage provider plugin (ADR 0036).
//!
//! One database, or a sharded cluster when the settings carry a shard map (ADR 0033),
//! in the capsule layout the settings choose (ADR 0034). Model queries compile to SQL
//! (`crate::model_query`); consensus logs live in `aseman_consensus` (ADR 0035).

use crate::consensus_log::PostgresConsensusLogStorage;
use crate::shard::{ShardMap, ShardedUnitOfWorkFactory, UnitOfWork, UnitOfWorkFactory, shard_of};
use crate::unit_of_work::PostgresUnitOfWorkFactory;
use crate::{
    PostgresCapsuleRepository, PostgresStorageError, import_prepared, is_reference_kind, layout,
    map_postgres_error, prepare_write, qualified, table_mapping,
};
use aseman_capsule::CapsuleStoreError;
use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleId, CapsuleKind};
use aseman_ports::consensus_log::ConsensusLogStorage;
use aseman_postgres::Database;
use aseman_storage::provider::{
    CapsuleTransaction, Mode, ProviderPlugin, ProviderSettings, StorageProvider,
};
use aseman_storage::schema::Model;
use aseman_storage::{FindMany, Id, StorageError, StorageResult, Where};
use postgres::Client;
use std::sync::Arc;
use uuid::Uuid;

/// The plugin name `ASEMAN_CORE_STORAGE_PROVIDER` selects.
pub const NAME: &str = "postgres";

fn storage_error(error: PostgresStorageError) -> StorageError {
    match error {
        PostgresStorageError::Invalid(message) => StorageError::Invalid(message),
        PostgresStorageError::Conflict => StorageError::conflict("revision or unique conflict"),
        PostgresStorageError::Unsupported(message) => StorageError::Unsupported(message),
        PostgresStorageError::Unavailable(message) => StorageError::Unavailable(message),
    }
}

fn capsule_store_error(error: CapsuleStoreError) -> StorageError {
    match error {
        CapsuleStoreError::Conflict => StorageError::conflict("revision or unique conflict"),
        CapsuleStoreError::Failed(message) if message.starts_with("invalid") => {
            StorageError::Invalid(message)
        }
        CapsuleStoreError::Failed(message) => StorageError::Unavailable(message),
    }
}

pub struct PostgresPlugin;

impl ProviderPlugin for PostgresPlugin {
    fn name(&self) -> &'static str {
        NAME
    }

    fn open(&self, settings: &ProviderSettings) -> StorageResult<Arc<dyn StorageProvider>> {
        let url = settings
            .database_url
            .clone()
            .ok_or_else(|| StorageError::invalid("the postgres provider needs a database URL"))?;
        PostgresCapsuleRepository::connect(&url)
            .and_then(|repository| repository.migrate_layout(settings.layout))
            .map_err(storage_error)?;
        let generation = Some(settings.binding_generation);
        let (factory, shards, home): (Arc<dyn UnitOfWorkFactory>, Vec<String>, usize) =
            match &settings.shard_map {
                Some(map) => {
                    let map = ShardMap::parse(map).map_err(storage_error)?;
                    let shards = map
                        .shards
                        .iter()
                        .map(|shard| shard.primary.clone())
                        .collect();
                    let home = map.home_index();
                    let factory = Arc::new(
                        ShardedUnitOfWorkFactory::connect(
                            map,
                            settings.max_connections,
                            generation,
                            settings.layout,
                        )
                        .map_err(storage_error)?,
                    );
                    (Arc::new(factory), shards, home)
                }
                None => (
                    Arc::new(
                        PostgresUnitOfWorkFactory::connect(
                            &url,
                            settings.max_connections,
                            generation,
                        )
                        .map_err(storage_error)?,
                    ),
                    vec![url.clone()],
                    0,
                ),
            };
        let logs =
            PostgresConsensusLogStorage::connect(&url, 4).map_err(StorageError::unavailable)?;
        let clock = Database::parse(&shards[home])
            .map_err(StorageError::unavailable)
            .and_then(|database| {
                aseman_postgres::pool(&database, 1).map_err(StorageError::unavailable)
            })?;
        Ok(Arc::new(PostgresProvider {
            factory,
            shards,
            home,
            logs: Arc::new(logs),
            clock,
        }))
    }
}

pub struct PostgresProvider {
    factory: Arc<dyn UnitOfWorkFactory>,
    /// Shard primaries in map order (one entry without a cluster).
    shards: Vec<String>,
    home: usize,
    logs: Arc<PostgresConsensusLogStorage>,
    /// A connection to the home shard for the database's clock.
    clock: aseman_postgres::Pool,
}

impl PostgresProvider {
    fn client(url: &str) -> StorageResult<Client> {
        Database::parse(url)
            .map_err(StorageError::unavailable)?
            .connect()
            .map_err(StorageError::unavailable)
    }

    /// The shards that hold `kind`: home for a reference kind, every shard otherwise.
    fn holders(&self, kind: &CapsuleKind) -> StorageResult<Vec<usize>> {
        Ok(if is_reference_kind(kind).map_err(storage_error)? {
            vec![self.home]
        } else {
            (0..self.shards.len()).collect()
        })
    }
}

impl StorageProvider for PostgresProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn now_millis(&self) -> StorageResult<i64> {
        self.clock
            .get()
            .map_err(StorageError::unavailable)?
            .query_one(
                "SELECT (extract(epoch from clock_timestamp()) * 1000)::bigint AS now",
                &[],
            )
            .map(|row| row.get("now"))
            .map_err(StorageError::unavailable)
    }

    fn begin(&self, mode: Mode) -> StorageResult<Box<dyn CapsuleTransaction>> {
        let unit = match mode {
            Mode::ReadWrite => self.factory.begin(),
            Mode::ReadOnly => self.factory.begin_read_only(),
        }
        .map_err(storage_error)?;
        Ok(Box::new(PostgresTransaction { unit, mode }))
    }

    fn consensus_logs(&self) -> Arc<dyn ConsensusLogStorage> {
        self.logs.clone()
    }

    fn export(
        &self,
        model: &Model,
        after: Option<Id>,
        limit: usize,
    ) -> StorageResult<Vec<CapsuleEnvelope>> {
        let kind = CapsuleKind(model.name.clone());
        let mapping = table_mapping(&kind).map_err(storage_error)?;
        let after = Uuid::from_bytes(after.map_or([0; 16], |id| id.0));
        let bound = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut capsules = Vec::new();
        for shard in self.holders(&kind)? {
            let mut client = Self::client(&self.shards[shard])?;
            let rows = client
                .query(
                    &format!(
                        "SELECT {} FROM {} WHERE id > $1 ORDER BY id LIMIT $2",
                        layout::select_list(mapping),
                        qualified(mapping)
                    ),
                    &[&after, &bound],
                )
                .map_err(|error| storage_error(map_postgres_error(error)))?;
            for row in &rows {
                capsules.push(layout::envelope_from_row(mapping, row).map_err(storage_error)?);
            }
        }
        capsules.sort_by(|left, right| left.id.cmp(&right.id));
        capsules.truncate(limit);
        Ok(capsules)
    }

    fn import(&self, model: &Model, capsules: &[CapsuleEnvelope]) -> StorageResult<()> {
        let kind = CapsuleKind(model.name.clone());
        let reference = is_reference_kind(&kind).map_err(storage_error)?;
        let layout = PostgresCapsuleRepository::connect(&self.shards[self.home])
            .map_err(storage_error)?
            .layout();
        for (shard, url) in self.shards.iter().enumerate() {
            let mut client = Self::client(url)?;
            let mut transaction = client.transaction().map_err(StorageError::unavailable)?;
            for capsule in capsules {
                // Reference kinds live on every shard; the others on their owner.
                if !reference && shard_of(&capsule.id, self.shards.len()) != shard {
                    continue;
                }
                let write = prepare_write(capsule, None, layout).map_err(storage_error)?;
                import_prepared(&mut transaction, &write).map_err(storage_error)?;
            }
            transaction.commit().map_err(StorageError::unavailable)?;
        }
        Ok(())
    }

    fn legacy_layout(&self) -> StorageResult<Option<String>> {
        let mut client = Self::client(&self.shards[self.home])?;
        for table in [
            "object_columns",
            "secondary_indexes",
            "relations",
            "documents",
            "opaque_values",
        ] {
            let present: bool = client
                .query_one(
                    &format!("SELECT to_regclass('aseman_compat.{table}') IS NOT NULL"),
                    &[],
                )
                .map_err(StorageError::unavailable)?
                .get(0);
            if !present {
                continue;
            }
            let rows: bool = client
                .query_one(
                    &format!("SELECT EXISTS (SELECT 1 FROM aseman_compat.{table})"),
                    &[],
                )
                .map_err(StorageError::unavailable)?
                .get(0);
            if rows {
                return Ok(Some("legacy key/value (aseman_compat)".to_owned()));
            }
        }
        // Consensus logs named by an engine's absolute directory (before ADR 0036).
        let absolute: bool = client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM aseman_consensus.log_entries \
                 WHERE starts_with(log, '/') AND strpos(log, '--UTC--') = 0)",
                &[],
            )
            .map_err(StorageError::unavailable)?
            .get(0);
        Ok(absolute.then(|| "legacy consensus log names (absolute paths)".to_owned()))
    }

    fn retire_legacy_layout(&self) -> StorageResult<()> {
        let mut client = Self::client(&self.shards[self.home])?;
        let present: bool = client
            .query_one("SELECT to_regnamespace('aseman_compat') IS NOT NULL", &[])
            .map_err(StorageError::unavailable)?
            .get(0);
        if present {
            let retired = format!(
                "aseman_compat_retired_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_secs())
            );
            client
                .batch_execute(&format!("ALTER SCHEMA aseman_compat RENAME TO {retired}"))
                .map_err(StorageError::unavailable)?;
        }
        Ok(())
    }
}

struct PostgresTransaction {
    unit: Box<dyn UnitOfWork>,
    mode: Mode,
}

impl CapsuleTransaction for PostgresTransaction {
    fn get(&self, model: &Model, id: Id) -> StorageResult<Option<CapsuleEnvelope>> {
        self.unit
            .get(&CapsuleKind(model.name.clone()), &CapsuleId(id.0))
            .map_err(capsule_store_error)
    }

    fn find(&self, model: &Model, query: &FindMany) -> StorageResult<Vec<CapsuleEnvelope>> {
        self.unit.find(&model.name, query).map_err(storage_error)
    }

    fn count(&self, model: &Model, filter: Option<&Where>) -> StorageResult<u64> {
        self.unit.count(&model.name, filter).map_err(storage_error)
    }

    fn put(
        &self,
        _model: &Model,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> StorageResult<()> {
        if self.mode == Mode::ReadOnly {
            return Err(StorageError::invalid("read-only transaction"));
        }
        self.unit
            .put(capsule, expected_revision)
            .map_err(capsule_store_error)
    }

    fn commit(&self) -> StorageResult<()> {
        self.unit.commit().map_err(storage_error)
    }

    fn rollback(&self) -> StorageResult<()> {
        self.unit.rollback().map_err(storage_error)
    }
}
