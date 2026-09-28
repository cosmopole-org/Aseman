//! The node's storage module (ADR 0036): the one door to the storage provider plugin
//! the node loaded.
//!
//! Every database operation of the node goes through here: transactions for state
//! actions, id minting, and the consensus logs. The provider is a
//! plugin chosen by `ASEMAN_CORE_STORAGE_PROVIDER` (see [`open`]); nothing in the node
//! names a provider or a key layout.

use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use aseman_storage::client::core::counter;
use aseman_storage::{Mode, Models, ProviderSettings, Registry, StorageError};
use uuid::Uuid;

use crate::core::trx::Trx;
use crate::models::ports::IStorage;

/// How many times id minting retries a lost race before failing.
const MINT_ATTEMPTS: usize = 16;

static INSTALLED: std::sync::OnceLock<aseman_storage::Storage> = std::sync::OnceLock::new();

/// Open the storage provider plugin `name` from `registry`, as the node's storage.
pub fn open(
    registry: &Registry,
    name: &str,
    settings: &ProviderSettings,
) -> Result<aseman_storage::Storage> {
    let storage = aseman_storage::Storage::open(registry, name, settings)
        .map_err(|error| anyhow!("{error}"))?;
    let _ = INSTALLED.set(storage.clone());
    Ok(storage)
}

/// The node's storage, once [`open`] ran (services composed after the node loads).
pub fn installed() -> Option<aseman_storage::Storage> {
    INSTALLED.get().cloned()
}

/// PostgreSQL connections the node's transactions may hold.
const STATE_CONNECTIONS: u32 = 16;
/// Where module administration listens when the provider does not serve it.
const DEFAULT_ADMIN_LISTEN: &str = "0.0.0.0:7440";

/// Open the storage provider plugin the configuration names (ADR 0036), with module
/// administration served on the provider's own listener (a RocksDB cluster) or on the
/// node's authenticated administration listener.
///
/// `serve_admin` is false for one-shot commands (a stopped node's operator tools).
pub fn open_from_config(
    config: Option<&aseman_config::AsemanConfig>,
    storage_root: &str,
    base_db_path: &str,
    serve_admin: bool,
) -> Result<aseman_storage::Storage> {
    use aseman_config::CoreStorageProvider;
    let routes = if serve_admin {
        crate::adapters::module_admin::route_handler(storage_root)
    } else {
        None
    };
    let mut settings = aseman_storage::ProviderSettings::embedded(storage_root)
        .map_err(|error| anyhow!("{error}"))?;
    settings.max_connections = STATE_CONNECTIONS;
    settings.admin_routes = routes.clone();
    // A RocksDB store from before ADR 0036 must be converted first.
    settings.legacy_store = Some(std::path::PathBuf::from(base_db_path));
    let name = match config {
        Some(config) => {
            settings.layout = config.core_storage.layout;
            settings.binding_generation = config.core_storage.binding_generation;
            settings.cluster = config.cluster.clone();
            if let Some(secret) = &config.database_url_secret {
                settings.database_url = Some(aseman_config::read_secret_file(secret, 4096)?);
            }
            if let Some(secret) = &config.core_storage.postgres_shards_secret {
                settings.shard_map = Some(aseman_config::read_secret_file(secret, 64 * 1024)?);
            }
            match config.core_storage.provider {
                CoreStorageProvider::Postgres => "postgres",
                CoreStorageProvider::RocksDb => "rocksdb",
            }
        }
        None => "rocksdb",
    };
    let storage = open(
        &aseman_storage_providers::registry(),
        name,
        &settings,
    )?;
    if !storage.provider().serves_admin_routes()
        && let Some(routes) = routes
    {
        let cluster = config.map(|config| &config.cluster);
        let token = cluster
            .and_then(|cluster| cluster.auth_token.clone())
            .unwrap_or_default();
        if !token.is_empty() {
            let listen = cluster
                .and_then(|cluster| cluster.listen_addr.clone())
                .unwrap_or_else(|| DEFAULT_ADMIN_LISTEN.to_owned());
            crate::adapters::module_admin::serve(routes, &listen, &token)?;
        }
    }
    Ok(storage)
}

/// Concrete [`IStorage`] implementation.
pub struct Storage {
    storage_root: String,
    storage: aseman_storage::Storage,
    mint: Mutex<()>,
}

impl Storage {
    /// Compose the node's storage over an opened provider.
    pub fn new(storage_root: &str, storage: aseman_storage::Storage) -> Arc<Storage> {
        Arc::new(Storage {
            storage_root: storage_root.to_string(),
            storage,
            mint: Mutex::new(()),
        })
    }

    fn mint(&self, name: &str) -> Result<i64> {
        let _guard = self.mint.lock().unwrap_or_else(|error| error.into_inner());
        for _ in 0..MINT_ATTEMPTS {
            let trx = self.begin(false)?;
            let next = trx
                .counter()
                .find_unique(counter::by_key(name))
                .map_err(|error| anyhow!("{error}"))?
                .map_or(1, |row| row.value + 1);
            let written = trx
                .counter()
                .upsert(
                    counter::by_key(name),
                    counter::Create {
                        key: name.to_owned(),
                        value: next,
                    },
                    counter::update().value(next),
                )
                .and_then(|_| trx.commit());
            match written {
                Ok(()) => return Ok(next),
                Err(StorageError::Conflict(_)) => continue,
                Err(error) => return Err(anyhow!("{error}")),
            }
        }
        Err(anyhow!("id counter {name} stayed contended"))
    }
}

impl IStorage for Storage {
    fn storage_root(&self) -> String {
        self.storage_root.clone()
    }

    fn begin(&self, readonly: bool) -> Result<Trx> {
        self.storage
            .begin(if readonly { Mode::ReadOnly } else { Mode::ReadWrite })
            .map_err(|error| anyhow!("storage: cannot begin a transaction: {error}"))
    }

    fn consensus_logs(&self) -> Arc<dyn aseman_ports::consensus_log::ConsensusLogStorage> {
        self.storage.provider().consensus_logs()
    }

    fn gen_id(&self, origin: &str) -> String {
        // Ids are `N@origin`: one counter for the global origin and one for every
        // local origin, as the node always minted them.
        let name = if origin == "global" { "global" } else { "local" };
        match self.mint(name) {
            Ok(value) => format!("{value}@{origin}"),
            Err(error) => {
                // A missed id is a failed action, not a reused one.
                eprintln!("storage: id minting failed: {error}");
                format!("{}@{origin}", Uuid::now_v7().simple())
            }
        }
    }
}
