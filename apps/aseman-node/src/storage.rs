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

/// How many times id minting retries a lost race before failing.
const MINT_ATTEMPTS: usize = 16;

/// Open the storage provider plugin `name` from `registry`, as the node's storage.
pub fn open(
    registry: &Registry,
    name: &str,
    settings: &ProviderSettings,
) -> Result<aseman_storage::Storage> {
    aseman_storage_providers::open(registry, name, settings).map_err(|error| anyhow!("{error}"))
}

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
    let routes = if serve_admin {
        crate::transports::admin::route_handler(storage_root)
    } else {
        None
    };
    let (name, mut settings) = match config {
        Some(config) => (
            aseman_storage_providers::provider_name(config.core_storage.provider),
            aseman_storage_providers::settings(
                config,
                &aseman_storage_providers::SecretOverrides::default(),
            )
            .map_err(|error| anyhow!("{error}"))?,
        ),
        None => {
            let mut settings = aseman_storage::ProviderSettings::embedded(storage_root)
                .map_err(|error| anyhow!("{error}"))?;
            // A RocksDB store from before ADR 0036 must be converted first.
            settings.legacy_store = Some(std::path::PathBuf::from(base_db_path));
            ("rocksdb", settings)
        }
    };
    settings.admin_routes = routes.clone();
    let storage = open(&aseman_storage_providers::registry(), name, &settings)?;
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
            // Module administration is served only over the cluster's mutual TLS.
            let files = cluster.and_then(|cluster| cluster.tls.as_ref()).ok_or_else(|| {
                anyhow!(
                    "module administration requires mutual TLS: set ASEMAN_CLUSTER_TLS_CERTIFICATE, \
                     ASEMAN_CLUSTER_TLS_KEY_SECRET, and ASEMAN_CLUSTER_TLS_CA"
                )
            })?;
            let tls = aseman_admin_http::MutualTls::load(files)
                .map_err(|error| anyhow!("module administration TLS: {error}"))?;
            aseman_admin_http::serve_routes(&listen, &tls, &token, "admin-https", routes)
                .map_err(|error| anyhow!(error))?;
        }
    }
    Ok(storage)
}

/// The node's storage: its root, the storage module, and id minting.
pub struct NodeStorage {
    storage_root: String,
    storage: aseman_storage::Storage,
    mint: Mutex<()>,
    /// The node master key, loaded or created on first use.
    master_key: std::sync::OnceLock<[u8; 32]>,
}

impl NodeStorage {
    /// Compose the node's storage over an opened provider.
    pub fn new(storage_root: &str, storage: aseman_storage::Storage) -> Arc<NodeStorage> {
        Arc::new(NodeStorage {
            storage_root: storage_root.to_string(),
            storage,
            mint: Mutex::new(()),
            master_key: std::sync::OnceLock::new(),
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

impl NodeStorage {
    pub(crate) fn storage_root(&self) -> String {
        self.storage_root.clone()
    }

    /// The storage module itself, for services that run their own transactions.
    pub(crate) fn storage(&self) -> aseman_storage::Storage {
        self.storage.clone()
    }

    /// The node master key secrets are encrypted under
    /// (`<storage_root>/node-secret-key`, created on first use).
    ///
    /// # Errors
    ///
    /// An unreadable or uncreatable key file.
    pub(crate) fn master_key(&self) -> Result<[u8; 32]> {
        if let Some(key) = self.master_key.get() {
            return Ok(*key);
        }
        let key = crate::util::secret_crypto::load_or_create_master_key(&self.storage_root)?;
        // A racing reader loaded or created the same file, so both hold one key.
        Ok(*self.master_key.get_or_init(|| key))
    }

    pub(crate) fn begin(&self, readonly: bool) -> Result<Trx> {
        self.storage
            .begin(if readonly {
                Mode::ReadOnly
            } else {
                Mode::ReadWrite
            })
            .map_err(|error| anyhow!("storage: cannot begin a transaction: {error}"))
    }

    pub(crate) fn consensus_logs(
        &self,
    ) -> Arc<dyn aseman_ports::consensus_log::ConsensusLogStorage> {
        self.storage.provider().consensus_logs()
    }

    pub(crate) fn gen_id(&self, origin: &str) -> String {
        // Ids are `N@origin`: one counter for the global origin and one for every
        // local origin, as the node always minted them.
        let name = if origin == "global" {
            "global"
        } else {
            "local"
        };
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

pub use aseman_storage::Trx;

/// A storage error as the `anyhow` error node actions return.
pub fn failed(error: aseman_storage::StorageError) -> anyhow::Error {
    anyhow::anyhow!("{error}")
}

/// An in-memory storage for tests: the reference provider over the model catalog.
#[cfg(test)]
pub(crate) fn test_storage() -> aseman_storage::Storage {
    aseman_storage::Storage::new(
        aseman_storage::memory::MemoryProvider::new(),
        aseman_storage::schema::Schema::catalog().expect("model catalog"),
    )
}

/// A read-write transaction on a fresh [`test_storage`].
#[cfg(test)]
pub(crate) fn test_trx() -> Trx {
    test_storage()
        .begin(aseman_storage::Mode::ReadWrite)
        .expect("memory transaction")
}
