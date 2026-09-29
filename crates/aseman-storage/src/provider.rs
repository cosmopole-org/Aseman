//! The provider plugin contract (ADR 0036).
//!
//! A provider stores capsules and answers model queries natively. The engine
//! (`crate::engine`) builds the Prisma-style API on top, so a provider implements only
//! reads, filtered queries, counts, and revision-checked writes.

use crate::error::{StorageError, StorageResult};
use crate::query::{FindMany, Where};
use crate::schema::{Model, Schema};
use crate::value::Id;
use aseman_config::{CapsuleLayout, ClusterBootstrapConfig, RocksDbTuning};
use aseman_contracts::capsule::CapsuleEnvelope;
use aseman_ports::consensus_log::ConsensusLogStorage;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Whether a transaction may write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    ReadWrite,
    ReadOnly,
}

/// One provider transaction over capsules.
pub trait CapsuleTransaction: Send + Sync {
    /// The capsule `id` of `model`, tombstones included (a re-create continues the
    /// revision chain).
    fn get(&self, model: &Model, id: Id) -> StorageResult<Option<CapsuleEnvelope>>;
    /// Live capsules matching `query`, ordered by `query.order_by` then id, after
    /// `skip`, at most `take` (the provider bounds an absent `take`).
    fn find(&self, model: &Model, query: &FindMany) -> StorageResult<Vec<CapsuleEnvelope>>;
    /// How many live capsules match `filter`.
    fn count(&self, model: &Model, filter: Option<&Where>) -> StorageResult<u64>;
    /// Write a sealed capsule. `expected_revision` is the stored revision it replaces
    /// (`None`: none is stored). A stale revision or a unique-index collision is
    /// `StorageError::Conflict`.
    fn put(
        &self,
        model: &Model,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> StorageResult<()>;
    fn commit(&self) -> StorageResult<()>;
    fn rollback(&self) -> StorageResult<()>;
}

/// An opened provider.
pub trait StorageProvider: Send + Sync {
    /// The plugin name (`rocksdb`, `postgres`, ...).
    fn name(&self) -> &str;
    fn begin(&self, mode: Mode) -> StorageResult<Box<dyn CapsuleTransaction>>;
    /// Where consensus engines keep their logs (ADR 0035).
    fn consensus_logs(&self) -> Arc<dyn ConsensusLogStorage>;
    /// Every stored capsule of `model`, tombstones included, in id order after
    /// `after`, at most `limit` (migration export).
    fn export(
        &self,
        model: &Model,
        after: Option<Id>,
        limit: usize,
    ) -> StorageResult<Vec<CapsuleEnvelope>>;
    /// Store capsules exactly as given (migration import); an identical replay
    /// succeeds, a different stored revision is a conflict.
    fn import(&self, model: &Model, capsules: &[CapsuleEnvelope]) -> StorageResult<()>;
    /// Whether this store still holds data in a layout the model API does not read
    /// (for example the legacy key/value layout); the node refuses to start on it.
    fn legacy_layout(&self) -> StorageResult<Option<String>> {
        Ok(None)
    }
    /// Set the legacy layout aside once `asemanctl storage migrate` converted it: it is
    /// kept, renamed, for inspection, and [`Self::legacy_layout`] no longer reports it.
    fn retire_legacy_layout(&self) -> StorageResult<()> {
        Ok(())
    }
    /// Whether the provider serves the settings' administration routes itself.
    fn serves_admin_routes(&self) -> bool {
        false
    }
}

/// Administration routes (`method`, `path`, `body`) -> `(status, body)`, answered
/// by the node; a provider with its own authenticated listener (the RocksDB
/// cluster) may serve them there.
pub type AdminRoutes = Arc<dyn Fn(&str, &str, &[u8]) -> Option<(u16, Vec<u8>)> + Send + Sync>;

/// What a plugin needs to open its provider.
#[derive(Clone)]
pub struct ProviderSettings {
    /// The node's storage root (embedded providers keep their files under it).
    pub storage_root: PathBuf,
    /// A database connection URL, already read from its secret.
    pub database_url: Option<String>,
    /// A cluster shard map (JSON), already read from its secret.
    pub shard_map: Option<String>,
    pub layout: CapsuleLayout,
    /// Writes are fenced at this binding generation (A309).
    pub binding_generation: u64,
    /// The RocksDB provider's replication settings.
    pub cluster: ClusterBootstrapConfig,
    /// The RocksDB provider's memory budget.
    pub rocksdb: RocksDbTuning,
    pub max_connections: u32,
    /// A store from before ADR 0036 that must be converted before this provider
    /// serves (the RocksDB provider's legacy key/value directory).
    pub legacy_store: Option<PathBuf>,
    /// Routes the provider may serve on its own listener.
    pub admin_routes: Option<AdminRoutes>,
    pub schema: Arc<Schema>,
}

impl std::fmt::Debug for dyn StorageProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

impl ProviderSettings {
    /// Settings for an embedded store under `storage_root` with the catalog schema.
    pub fn embedded(storage_root: impl Into<PathBuf>) -> StorageResult<Self> {
        Ok(Self {
            storage_root: storage_root.into(),
            database_url: None,
            shard_map: None,
            layout: CapsuleLayout::default(),
            binding_generation: 0,
            cluster: ClusterBootstrapConfig::default(),
            rocksdb: RocksDbTuning::default(),
            max_connections: 8,
            legacy_store: None,
            admin_routes: None,
            schema: Schema::catalog()?,
        })
    }
}

/// A storage provider plugin, registered by name.
pub trait ProviderPlugin: Send + Sync {
    fn name(&self) -> &'static str;
    fn open(&self, settings: &ProviderSettings) -> StorageResult<Arc<dyn StorageProvider>>;
}

/// The plugins a process can load.
#[derive(Clone, Default)]
pub struct Registry {
    plugins: BTreeMap<String, Arc<dyn ProviderPlugin>>,
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, plugin: Arc<dyn ProviderPlugin>) -> &mut Self {
        self.plugins.insert(plugin.name().to_owned(), plugin);
        self
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.plugins.keys().map(String::as_str)
    }

    /// Open the plugin named `name`.
    pub fn open(
        &self,
        name: &str,
        settings: &ProviderSettings,
    ) -> StorageResult<Arc<dyn StorageProvider>> {
        let plugin = self.plugins.get(name).ok_or_else(|| {
            StorageError::invalid(format!(
                "no storage provider plugin named {name} (available: {})",
                self.plugins.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
        })?;
        plugin.open(settings)
    }
}
