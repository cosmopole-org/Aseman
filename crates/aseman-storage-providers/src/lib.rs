//! The storage provider plugins linked into Aseman executables (ADR 0036).
//!
//! Composition roots (the node, asemanctl) call [`registry`] and open the plugin the
//! configuration names; nothing else names a provider.
#![forbid(unsafe_code)]

use aseman_config::{AsemanConfig, CoreStorageProvider};
use aseman_storage::{ProviderSettings, Registry, StorageError, StorageResult};
use std::path::PathBuf;
use std::sync::Arc;

pub mod migrate;

/// Every provider plugin: `postgres` and `rocksdb`.
#[must_use]
pub fn registry() -> Registry {
    let mut registry = Registry::new();
    registry
        .register(Arc::new(aseman_storage_postgres::plugin::PostgresPlugin))
        .register(Arc::new(aseman_storage_rocksdb::model_store::RocksDbPlugin));
    registry
}

/// PostgreSQL connections the node's transactions may hold.
const STATE_CONNECTIONS: u32 = 16;

/// The plugin name of a configured provider.
#[must_use]
pub fn provider_name(provider: CoreStorageProvider) -> &'static str {
    match provider {
        CoreStorageProvider::Postgres => aseman_storage_postgres::plugin::NAME,
        CoreStorageProvider::RocksDb => aseman_storage_rocksdb::model_store::NAME,
    }
}

/// Secret overrides for a provider other than the configured one.
#[derive(Clone, Debug, Default)]
pub struct SecretOverrides {
    /// A file holding the database URL (`--database-url-secret`).
    pub database_url_secret: Option<PathBuf>,
    /// A file holding a PostgreSQL shard map (`--shards-secret`).
    pub shards_secret: Option<PathBuf>,
}

/// The settings `config` gives a provider, with its secrets read. Overrides replace
/// the configured secrets (a migration target).
///
/// # Errors
///
/// An unreadable secret.
pub fn settings(
    config: &AsemanConfig,
    overrides: &SecretOverrides,
) -> StorageResult<ProviderSettings> {
    let read = |path: &std::path::Path, max: usize| {
        aseman_config::read_secret_file(path, max)
            .map_err(|error| StorageError::invalid(format!("secret {}: {error}", path.display())))
    };
    let mut settings = ProviderSettings::embedded(&config.storage.root_path)?;
    settings.max_connections = STATE_CONNECTIONS;
    settings.layout = config.core_storage.layout;
    settings.binding_generation = config.core_storage.binding_generation;
    settings.cluster = config.cluster.clone();
    settings.rocksdb = config.legacy_adapters.rocksdb;
    // A RocksDB store from before ADR 0036 must be converted first.
    settings.legacy_store = Some(PathBuf::from(&config.storage.base_db_path));
    let database_url = overrides
        .database_url_secret
        .clone()
        .or_else(|| config.database_url_secret.as_ref().map(PathBuf::from));
    if let Some(secret) = database_url {
        settings.database_url = Some(read(&secret, 4096)?);
    }
    let shards = overrides.shards_secret.clone().or_else(|| {
        config
            .core_storage
            .postgres_shards_secret
            .as_ref()
            .map(PathBuf::from)
    });
    if let Some(secret) = shards {
        settings.shard_map = Some(read(&secret, 64 * 1024)?);
    }
    Ok(settings)
}

/// Readers of stores from before ADR 0036, for the one-shot legacy migration commands
/// (`asemanctl storage migrate`, the node's `vmm-handoff`).
pub mod legacy {
    pub use aseman_storage_rocksdb::{
        LegacyKvStore, LegacyVmDecision, LegacyVmHandoffDecisions, LegacyVmHandoffPlan,
        RocksDbKvStore, check_legacy_vm_decisions, complete_legacy_vm_handoff,
        plan_legacy_vm_handoff,
    };
}

/// The trusted guest proxy a creature's own database is reached through (A306/A405).
pub struct GuestProxy<'a> {
    /// The proxy's connection URL on the home database server.
    pub url: &'a str,
    pub role: &'a str,
    pub max_pools: usize,
    pub max_pool_size: u32,
    /// A cluster shard map (JSON): each shard's own guest proxy serves the creatures
    /// placed on it (ADR 0033).
    pub shard_map: Option<&'a str>,
}

/// Guest data served from each creature's own database (ADR 0021): the PostgreSQL
/// guest data plane, whichever provider holds the node's models.
pub fn guest_kv(proxy: &GuestProxy<'_>) -> Result<Arc<dyn aseman_ports::GuestKv>, String> {
    use aseman_storage_postgres::guest::{GuestPoolRouter, PostgresGuestKv};
    let mut router =
        GuestPoolRouter::new(proxy.url, proxy.role, proxy.max_pools, proxy.max_pool_size)
            .map_err(|error| error.to_string())?;
    if let Some(map) = proxy.shard_map {
        let map = aseman_storage_postgres::shard::ShardMap::parse(map)
            .map_err(|error| error.to_string())?;
        for shard in &map.shards {
            if let Some(guest_proxy) = &shard.guest_proxy {
                router = router
                    .with_shard_proxy(&shard.name, guest_proxy)
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(Arc::new(PostgresGuestKv::new(router)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn both_providers_are_registered() {
        let registry = super::registry();
        assert_eq!(
            registry.names().collect::<Vec<_>>(),
            ["postgres", "rocksdb"]
        );
    }
}
