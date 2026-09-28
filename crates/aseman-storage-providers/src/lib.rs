//! The storage provider plugins linked into Aseman executables (ADR 0036).
//!
//! Composition roots (the node, asemanctl) call [`registry`] and open the plugin the
//! configuration names; nothing else names a provider.
#![forbid(unsafe_code)]

use aseman_storage::Registry;
use std::sync::Arc;

/// Every provider plugin: `postgres` and `rocksdb`.
#[must_use]
pub fn registry() -> Registry {
    let mut registry = Registry::new();
    registry
        .register(Arc::new(aseman_storage_postgres::plugin::PostgresPlugin))
        .register(Arc::new(aseman_storage_rocksdb::model_store::RocksDbPlugin));
    registry
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
    let mut router = GuestPoolRouter::new(
        proxy.url,
        proxy.role,
        proxy.max_pools,
        proxy.max_pool_size,
    )
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
        assert_eq!(registry.names().collect::<Vec<_>>(), ["postgres", "rocksdb"]);
    }
}
