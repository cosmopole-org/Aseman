//! Translation of `chain/babble/babble.go`.
//!
//! `Babble` is the top-level engine that glues a `Config`, `Store`,
//! `Transport`, `Proxy` and `Node` together. The Go implementation also wired
//! in a WebRTC/WAMP transport and a debug HTTP `service.Service`; both are
//! intentionally dropped in the Rust port — the only network transport offered
//! is the custom-TCP `NetworkTransport` from `chain::net`, and the debug HTTP
//! service is not translated (the node's stats are available directly through
//! `Node` accessors).

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};

use crate::compat::logrus::Entry;
use crate::drivers::network::chain::config::Config;
use crate::drivers::network::chain::crypto::keys::{KeyReaderWriter, SimpleKeyfile};
use crate::drivers::network::chain::hashgraph::{InmemStore, RocksDbStore, Store};
use crate::drivers::network::chain::net::{new_tcp_transport, Transport};
use crate::drivers::network::chain::node::{Node, Validator};
use crate::drivers::network::chain::peers::{JSONPeerSet, PeerSet};

/// Encapsulates the components that make up a Babble node.
pub struct Babble {
    pub config: Arc<Config>,
    pub node: Option<Arc<Node>>,
    pub transport: Option<Arc<dyn Transport>>,
    pub store: Option<Box<dyn Store>>,
    pub peers: Option<Arc<PeerSet>>,
    pub genesis_peers: Option<Arc<PeerSet>>,
    pub logger: Entry,
}

impl Babble {
    /// Returns a new `Babble` engine.
    pub fn new(config: Arc<Config>) -> Babble {
        let logger = config.logger();
        Babble {
            config,
            node: None,
            transport: None,
            store: None,
            peers: None,
            genesis_peers: None,
            logger,
        }
    }

    /// Initialises the engine in dependency order: config validation, key,
    /// peers, store, transport, node. The `transport` argument lets the caller
    /// inject an out-of-band transport (used by tests); pass `None` to build
    /// a TCP transport from the config.
    pub fn init(
        &mut self,
        transport: Option<Arc<dyn Transport>>,
        work_chain_id: &str,
        shard_chain_id: &str,
        on_new_node_cb: Option<Box<dyn Fn(String) + Send + Sync>>,
    ) -> Result<()> {
        self.logger.debug("validateConfig");
        if let Err(e) = self.validate_config() {
            self.logger
                .with_error(&e)
                .error("babble.rs:init() validate_config");
        }

        self.logger.debug("initKey");
        self.init_key().map_err(|e| {
            self.logger
                .with_error(&e)
                .error("babble.rs:init() init_key");
            e
        })?;

        self.logger.debug("initPeers");
        self.init_peers().map_err(|e| {
            self.logger
                .with_error(&e)
                .error("babble.rs:init() init_peers");
            e
        })?;

        self.logger.debug("initStore");
        self.init_store().map_err(|e| {
            self.logger
                .with_error(&e)
                .error("babble.rs:init() init_store");
            e
        })?;

        self.logger.debug("initTransport");
        if let Some(t) = transport {
            self.transport = Some(t);
        } else if !self.config.maintenance_mode {
            self.init_transport().map_err(|e| {
                self.logger
                    .with_error(&e)
                    .error("babble.rs:init() init_transport");
                e
            })?;
        }

        self.logger.debug("initNode");
        self.init_node(work_chain_id, shard_chain_id, on_new_node_cb)
            .map_err(|e| {
                self.logger
                    .with_error(&e)
                    .error("babble.rs:init() init_node");
                e
            })?;

        Ok(())
    }

    /// Starts the Babble node — runs the gossip loop.
    pub fn run(&self) {
        if let Some(n) = &self.node {
            n.run(true);
        }
    }

    /// Attach an external peer-host mirror to the consensus node so it tracks
    /// dynamic membership and can be read without locking the engine/core.
    /// See `Core::peer_host_cache`.
    pub fn attach_peer_cache(&self, cache: Arc<std::sync::Mutex<Vec<String>>>) {
        if let Some(n) = &self.node {
            n.attach_peer_cache(cache);
        }
    }

    fn validate_config(&self) -> Result<()> {
        // Note: SetDataDir would mutate the config; the Rust translation keeps
        // Config behind an `Arc` so callers configure it before constructing
        // the engine. The remaining validation only logs.

        // Maintenance-mode only works with bootstrap; bootstrap only works
        // with store. We can't mutate the shared `Arc<Config>` here, so we
        // surface this as a hard error if the combination is wrong.
        if self.config.maintenance_mode && !self.config.bootstrap {
            return Err(anyhow!("maintenance-mode requires bootstrap to be enabled"));
        }
        if self.config.bootstrap && !self.config.store {
            return Err(anyhow!("bootstrap requires store to be enabled"));
        }
        if self.config.slow_heartbeat_timeout < self.config.heartbeat_timeout {
            self.logger.debug(format!(
                "SlowHeartbeatTimeout ({:?}) cannot be less than Heartbeat ({:?})",
                self.config.slow_heartbeat_timeout, self.config.heartbeat_timeout
            ));
            // Caller can fix this by constructing the config correctly; we
            // don't try to mutate `Arc<Config>` from here.
        }
        Ok(())
    }

    fn init_transport(&mut self) -> Result<()> {
        // WebRTC support is dropped; only TCP is offered.
        let timeout = self.config.tcp_timeout;
        let join_timeout = self.config.join_timeout;
        let max_pool = if self.config.max_pool <= 0 {
            1
        } else {
            self.config.max_pool as usize
        };
        let trans = new_tcp_transport(
            &self.config.bind_addr,
            &self.config.advertise_addr,
            max_pool,
            timeout,
            join_timeout,
            Some(self.logger.clone()),
        )?;
        self.transport = Some(trans);
        Ok(())
    }

    fn init_peers(&mut self) -> Result<()> {
        let peer_store = JSONPeerSet::new(&self.config.data_dir, true);
        let participants = peer_store
            .peer_set()?
            .ok_or_else(|| anyhow!("peers.json: no participants found"))?;
        let participants = Arc::new(participants);
        self.peers = Some(participants.clone());

        // Genesis peer-set is optional — fall back to the current set.
        let genesis_store = JSONPeerSet::new(&self.config.data_dir, false);
        let genesis = match genesis_store.peer_set() {
            Ok(Some(g)) => Arc::new(g),
            _ => {
                self.logger
                    .debug("could not read peers.genesis.json; using current set");
                participants
            }
        };
        self.genesis_peers = Some(genesis);
        Ok(())
    }

    fn init_store(&mut self) -> Result<()> {
        if !self.config.store {
            self.logger.debug("Creating InmemStore");
            self.store = Some(Box::new(InmemStore::new(self.config.cache_size)));
        } else {
            let db_path = &self.config.database_dir;
            self.logger
                .with_field("path", db_path)
                .debug("Creating RocksDB store");

            if !self.config.bootstrap {
                let backup = backup_file_name(db_path);
                match fs::rename(db_path, &backup) {
                    Ok(()) => self
                        .logger
                        .with_field("path", &backup)
                        .debug("Created backup"),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        self.logger.debug("Nothing to backup");
                    }
                    Err(e) => return Err(anyhow!("backup db dir: {}", e)),
                }
            }

            let store = RocksDbStore::new(
                self.config.cache_size,
                db_path,
                self.config.maintenance_mode,
            )?;
            self.store = Some(Box::new(store));
        }
        Ok(())
    }

    fn init_key(&mut self) -> Result<()> {
        if self.config.key.is_some() {
            return Ok(());
        }
        // We can't mutate `Arc<Config>`. Callers that don't pre-load the key
        // must pass a `Config` with `key: Some(...)`, or rely on
        // `load_key_for_config` below to construct one before wrapping it in
        // the Arc.
        Err(anyhow!(
            "Config.key is unset; pre-load the validator key with \
             `babble::load_key_for_config` before constructing Babble"
        ))
    }

    fn init_node(
        &mut self,
        work_chain_id: &str,
        shard_chain_id: &str,
        on_new_node_cb: Option<Box<dyn Fn(String) + Send + Sync>>,
    ) -> Result<()> {
        let key = self
            .config
            .key
            .clone()
            .ok_or_else(|| anyhow!("Config.key is unset"))?;
        let mut validator = Validator::new(key, &self.config.moniker);

        // Borrow moniker override from peers.json, matching Go behaviour.
        if let Some(peers) = &self.peers {
            if let Some(p) = peers.by_id.get(&validator.id()) {
                if p.moniker != validator.moniker {
                    self.logger.debug(format!(
                        "Using moniker `{}` from peers.json (was `{}`)",
                        p.moniker, validator.moniker
                    ));
                    validator.moniker = p.moniker.clone();
                }
            }
        }

        let peers = self
            .peers
            .clone()
            .ok_or_else(|| anyhow!("init_peers must run before init_node"))?;
        let genesis_peers = self
            .genesis_peers
            .clone()
            .ok_or_else(|| anyhow!("init_peers must run before init_node"))?;
        let store = self
            .store
            .take()
            .ok_or_else(|| anyhow!("init_store must run before init_node"))?;
        let transport = self
            .transport
            .clone()
            .ok_or_else(|| anyhow!("init_transport must run before init_node"))?;
        let proxy = self
            .config
            .proxy
            .clone()
            .ok_or_else(|| anyhow!("Config.proxy must be set before init_node"))?;

        let on_new_node: Box<dyn Fn(String) + Send + Sync> =
            on_new_node_cb.unwrap_or_else(|| Box::new(|_| {}));

        let node = Node::new(
            self.config.clone(),
            validator,
            peers,
            genesis_peers,
            store,
            transport,
            proxy,
            work_chain_id,
            shard_chain_id,
            on_new_node,
        );
        node.init()?;
        self.node = Some(node);
        Ok(())
    }
}

/// Loads the validator key from disk and writes it into `config.key`.
///
/// Use this before wrapping `Config` in `Arc` so the loaded key is visible to
/// the [`Babble`] engine via the shared `Arc<Config>`.
pub fn load_key_for_config(config: &mut Config) -> Result<()> {
    if config.key.is_some() {
        return Ok(());
    }
    let keyfile_path = config.keyfile();
    let keyfile = SimpleKeyfile::new(&keyfile_path);

    if !std::path::Path::new(&keyfile_path).exists() {
        // Auto-generate on first boot so the node can start without manual key pre-provisioning.
        let new_key = crate::drivers::network::chain::crypto::keys::generate_ecdsa_key()
            .map_err(|e| anyhow!("generate key: {}", e))?;
        keyfile
            .write_key(&new_key)
            .map_err(|e| anyhow!("write generated key to {}: {}", keyfile_path, e))?;

        // Also write key.pub next to the shard's priv_key. Without it,
        // anything that reads the shard directory to rebuild a peer set
        // (other follower nodes, `casparctl peers`, etc.) has no way to
        // know which PubKeyHex this validator owns.
        let derived_pub =
            crate::drivers::network::chain::crypto::keys::public_key_hex(new_key.verifying_key());
        if let Some(parent) = std::path::Path::new(&keyfile_path).parent() {
            let _ = fs::write(parent.join("key.pub"), derived_pub.as_bytes());
        }

        // Mirror the key pair to BABBLE_DATA_DIR so shardchain.sh can copy
        // it on subsequent shard bootstraps. We must mirror BOTH priv_key
        // and key.pub — mirroring priv_key alone silently invalidates any
        // peers.genesis.json that was pre-built from the old key.pub, and
        // every node downstream then sees a validator id that doesn't match
        // its own peer entry, gets stuck in JOINING forever, and never
        // opens its TCP API listener.
        if let Some(babble_dir) = aseman_config::legacy_adapter_snapshot()
            .and_then(|config| config.babble_data_dir.as_ref())
        {
            let _ = fs::create_dir_all(&babble_dir);
            let mirror_priv = SimpleKeyfile::new(&format!("{}/priv_key", babble_dir));
            let _ = mirror_priv.write_key(&new_key);
            let _ = fs::write(format!("{}/key.pub", babble_dir), derived_pub.as_bytes());
        }

        config.key = Some(new_key);
        return Ok(());
    }

    let key = keyfile.read_key().map_err(|e| {
        anyhow!(
            "Error reading private key from file {}: {}",
            keyfile_path,
            e
        )
    })?;
    config.key = Some(key);
    Ok(())
}

/// `<base>--UTC--<ts>` — matches Go's backup naming convention.
fn backup_file_name(base: &str) -> String {
    format!("{}--UTC--{}", base, to_iso8601(Utc::now()))
}

fn to_iso8601(t: DateTime<Utc>) -> String {
    use chrono::Datelike;
    use chrono::Timelike;
    format!(
        "{:04}-{:02}-{:02}T{:02}-{:02}-{:02}.{:09}Z",
        t.year(),
        t.month(),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.nanosecond()
    )
}

// Unused-import suppression in case `Path` becomes needed later.
const _: fn() -> Option<&'static Path> = || None;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::network::chain::crypto::keys::{
        dump_private_key, generate_ecdsa_key, parse_private_key,
    };

    fn tmp_data_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("babble-{}-{}-{}", label, std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_key_for_config_is_no_op_when_key_already_set() {
        let mut cfg = Config::new_default_config("127.0.0.1:1");
        cfg.data_dir = tmp_data_dir("load-noop").to_string_lossy().into_owned();
        let key = generate_ecdsa_key().unwrap();
        let expected_d = dump_private_key(&key);
        cfg.key = Some(key);

        // No keyfile exists, but `load_key_for_config` must not touch the
        // existing key — and must not error.
        load_key_for_config(&mut cfg).expect("noop when key set");
        assert_eq!(dump_private_key(cfg.key.as_ref().unwrap()), expected_d);
        let _ = std::fs::remove_dir_all(&cfg.data_dir);
    }

    #[test]
    fn load_key_for_config_reads_from_disk_when_keyfile_present() {
        let dir = tmp_data_dir("load-disk");
        let mut cfg = Config::new_default_config("127.0.0.1:1");
        cfg.data_dir = dir.to_string_lossy().into_owned();

        // Write a key to `<data_dir>/priv_key` using the same writer Babble
        // uses to load it.
        let key = generate_ecdsa_key().unwrap();
        let keyfile = SimpleKeyfile::new(&cfg.keyfile());
        keyfile.write_key(&key).expect("write key");

        load_key_for_config(&mut cfg).expect("load");
        let loaded = cfg.key.as_ref().expect("key populated");
        assert_eq!(
            dump_private_key(loaded),
            dump_private_key(&key),
            "loaded key D must match the one written"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_key_for_config_auto_generates_key_when_keyfile_missing() {
        let dir = tmp_data_dir("load-missing");
        let mut cfg = Config::new_default_config("127.0.0.1:1");
        cfg.data_dir = dir.to_string_lossy().into_owned();
        // On first boot with no keyfile present, a key is auto-generated and
        // written to disk so the node can start without manual provisioning.
        load_key_for_config(&mut cfg).expect("auto-generation should succeed");
        assert!(cfg.key.is_some(), "key should be set after auto-generation");
        // The keyfile should now exist on disk.
        let keyfile_path = cfg.keyfile();
        assert!(
            std::path::Path::new(&keyfile_path).exists(),
            "keyfile should have been written to disk"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_file_name_appends_utc_timestamp_suffix() {
        let name = backup_file_name("peers.json");
        // Format: "<base>--UTC--<YYYY-MM-DDThh-mm-ss.nnnnnnnnnZ>".
        assert!(name.starts_with("peers.json--UTC--"));
        let suffix = &name["peers.json--UTC--".len()..];
        assert!(suffix.ends_with('Z'));
        assert!(suffix.contains('T'));
    }

    #[test]
    fn to_iso8601_format_is_path_safe() {
        let t = chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap();
        let s = to_iso8601(t);
        // Path-safe representation (no colons) of the unix epoch.
        assert_eq!(s, "1970-01-01T00-00-00.000000000Z");
    }

    #[test]
    fn parse_private_key_round_trip_through_load() {
        // Sanity: the key produced by `generate_ecdsa_key` survives a
        // `dump -> parse` round trip — used by `load_key_for_config`.
        let key = generate_ecdsa_key().unwrap();
        let bytes = dump_private_key(&key);
        let parsed = parse_private_key(&bytes).expect("parse");
        assert_eq!(dump_private_key(&parsed), bytes);
    }
}
