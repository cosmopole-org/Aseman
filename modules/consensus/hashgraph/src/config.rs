//! Translation of `chain/config/config.go`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use k256::ecdsa::SigningKey;

use crate::logrus::{Entry, Level, Logger};
use crate::proxy::AppProxy;

/// Default name of the file containing the validator's private key.
pub const DEFAULT_KEYFILE: &str = "priv_key";
/// Default name of the folder containing the database.
pub const DEFAULT_DB_FILE: &str = "rocksdb_db";
/// Default name of the signal-server TLS certificate file.
pub const DEFAULT_CERT_FILE: &str = "cert.pem";

pub const DEFAULT_LOG_LEVEL: &str = "debug";
pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:1337";
pub const DEFAULT_SERVICE_ADDR: &str = "0.0.0.0:8000";
pub const DEFAULT_CACHE_SIZE: i64 = 10000;
pub const DEFAULT_SYNC_LIMIT: i64 = 1000;
pub const DEFAULT_MAX_POOL: i64 = 2;
pub const DEFAULT_STORE: bool = true;
pub const DEFAULT_MAINTENANCE_MODE: bool = false;
pub const DEFAULT_SUSPEND_LIMIT: i64 = 10000;
pub const DEFAULT_WEBRTC: bool = false;
pub const DEFAULT_SIGNAL_ADDR: &str = "127.0.0.1:2443";
pub const DEFAULT_SIGNAL_REALM: &str = "main";
pub const DEFAULT_SIGNAL_SKIP_VERIFY: bool = false;
pub const DEFAULT_ICE_ADDRESS: &str = "stun:stun.l.google.com:19302";
pub const DEFAULT_ICE_USERNAME: &str = "";
pub const DEFAULT_ICE_PASSWORD: &str = "";

fn default_heartbeat_timeout() -> Duration {
    // Gossip period: how often the babble loop dispatches a gossip cycle when
    // the node is busy. Reduced 20x (from 200ms) to tighten propagation latency.
    Duration::from_millis(10)
}
fn default_slow_heartbeat_timeout() -> Duration {
    Duration::from_millis(1000)
}
fn default_tcp_timeout() -> Duration {
    Duration::from_millis(1000)
}
fn default_join_timeout() -> Duration {
    Duration::from_millis(10000)
}

/// A server providing ICE services (STUN/TURN), the translation of
/// `webrtc.ICEServer`.
#[derive(Debug, Clone, Default)]
pub struct IceServer {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub credential_type: String,
}

/// All the configuration properties of a Babble node.
#[derive(Clone)]
pub struct Config {
    pub data_dir: String,
    pub log_level: String,
    pub bind_addr: String,
    pub advertise_addr: String,
    pub no_service: bool,
    pub service_addr: String,
    pub heartbeat_timeout: Duration,
    pub slow_heartbeat_timeout: Duration,
    pub max_pool: i64,
    pub tcp_timeout: Duration,
    pub join_timeout: Duration,
    pub sync_limit: i64,
    pub enable_fast_sync: bool,
    pub store: bool,
    pub database_dir: String,
    pub cache_size: i64,
    pub bootstrap: bool,
    pub maintenance_mode: bool,
    pub suspend_limit: i64,
    pub moniker: String,
    pub webrtc: bool,
    pub signal_addr: String,
    pub signal_realm: String,
    pub signal_skip_verify: bool,
    pub ice_address: String,
    pub ice_username: String,
    pub ice_password: String,
    /// The application proxy that lets Babble communicate with the app.
    pub proxy: Option<Arc<dyn AppProxy>>,
    /// The private key of the validator.
    pub key: Option<SigningKey>,
}

impl Config {
    /// Returns a config object with default values.
    pub fn new_default_config(adv_addr: &str) -> Config {
        Config {
            advertise_addr: adv_addr.to_string(),
            data_dir: default_data_dir(),
            log_level: DEFAULT_LOG_LEVEL.to_string(),
            bind_addr: DEFAULT_BIND_ADDR.to_string(),
            service_addr: DEFAULT_SERVICE_ADDR.to_string(),
            heartbeat_timeout: default_heartbeat_timeout(),
            slow_heartbeat_timeout: default_slow_heartbeat_timeout(),
            tcp_timeout: default_tcp_timeout(),
            join_timeout: default_join_timeout(),
            cache_size: DEFAULT_CACHE_SIZE,
            sync_limit: DEFAULT_SYNC_LIMIT,
            max_pool: DEFAULT_MAX_POOL,
            store: DEFAULT_STORE,
            maintenance_mode: DEFAULT_MAINTENANCE_MODE,
            database_dir: default_database_dir(),
            suspend_limit: DEFAULT_SUSPEND_LIMIT,
            webrtc: DEFAULT_WEBRTC,
            signal_addr: DEFAULT_SIGNAL_ADDR.to_string(),
            signal_realm: DEFAULT_SIGNAL_REALM.to_string(),
            signal_skip_verify: DEFAULT_SIGNAL_SKIP_VERIFY,
            ice_address: DEFAULT_ICE_ADDRESS.to_string(),
            ice_username: DEFAULT_ICE_USERNAME.to_string(),
            ice_password: DEFAULT_ICE_PASSWORD.to_string(),
            no_service: false,
            enable_fast_sync: false,
            bootstrap: false,
            moniker: String::new(),
            proxy: None,
            key: None,
        }
    }

    /// Sets the top-level Babble directory, updating the database directory if
    /// it is still at its default value.
    pub fn set_data_dir(&mut self, data_dir: &str) {
        self.data_dir = data_dir.to_string();
        if self.database_dir == default_database_dir() {
            self.database_dir = PathBuf::from(data_dir)
                .join(DEFAULT_DB_FILE)
                .to_string_lossy()
                .into_owned();
        }
    }

    /// The full path of the file containing the private key.
    pub fn keyfile(&self) -> String {
        PathBuf::from(&self.data_dir)
            .join(DEFAULT_KEYFILE)
            .to_string_lossy()
            .into_owned()
    }

    /// The full path of the signal-server TLS certificate file.
    pub fn cert_file(&self) -> String {
        PathBuf::from(&self.data_dir)
            .join(DEFAULT_CERT_FILE)
            .to_string_lossy()
            .into_owned()
    }

    /// The list of ICE servers used by the WebRTC stream layer.
    pub fn ice_servers(&self) -> Vec<IceServer> {
        vec![IceServer {
            urls: vec![self.ice_address.clone()],
            username: self.ice_username.clone(),
            credential: self.ice_password.clone(),
            credential_type: "password".to_string(),
        }]
    }

    /// Returns a formatted logrus `Entry` with prefix "babble".
    pub fn logger(&self) -> Entry {
        let logger = Logger::new();
        logger.set_level(log_level(&self.log_level));
        Entry::new(logger).with_field("prefix", "babble")
    }
}

/// The default path for the database files.
pub fn default_database_dir() -> String {
    PathBuf::from(default_data_dir())
        .join(DEFAULT_DB_FILE)
        .to_string_lossy()
        .into_owned()
}

/// The default directory for top-level Babble config, based on the OS.
pub fn default_data_dir() -> String {
    let home = home_dir();
    if !home.is_empty() {
        let p = match std::env::consts::OS {
            "macos" => PathBuf::from(&home).join(".Babble"),
            "windows" => PathBuf::from(&home)
                .join("AppData")
                .join("Roaming")
                .join("Babble"),
            _ => PathBuf::from(&home).join(".babble"),
        };
        return p.to_string_lossy().into_owned();
    }
    String::new()
}

/// The user's home directory.
pub fn home_dir() -> String {
    if let Some(config) = aseman_config::legacy_adapter_snapshot() {
        if let Some(home) = &config.home_dir {
            return home.clone();
        }
        if let Some(profile) = &config.user_profile_dir {
            return profile.clone();
        }
    }
    String::new()
}

/// Parses a string into a logrus log level.
pub fn log_level(l: &str) -> Level {
    match l {
        "debug" => Level::Debug,
        "info" => Level::Info,
        "warn" => Level::Warn,
        "error" => Level::Error,
        "fatal" => Level::Fatal,
        "panic" => Level::Panic,
        _ => Level::Debug,
    }
}

/// The default ICE configuration, with one public Google STUN server.
pub fn default_ice_servers() -> Vec<IceServer> {
    vec![IceServer {
        urls: vec![DEFAULT_ICE_ADDRESS.to_string()],
        ..IceServer::default()
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_default_config_uses_documented_defaults() {
        let c = Config::new_default_config("10.0.0.1:9000");
        assert_eq!(c.advertise_addr, "10.0.0.1:9000");
        assert_eq!(c.log_level, DEFAULT_LOG_LEVEL);
        assert_eq!(c.bind_addr, DEFAULT_BIND_ADDR);
        assert_eq!(c.service_addr, DEFAULT_SERVICE_ADDR);
        assert_eq!(c.cache_size, DEFAULT_CACHE_SIZE);
        assert_eq!(c.sync_limit, DEFAULT_SYNC_LIMIT);
        assert_eq!(c.max_pool, DEFAULT_MAX_POOL);
        assert_eq!(c.store, DEFAULT_STORE);
        assert_eq!(c.maintenance_mode, DEFAULT_MAINTENANCE_MODE);
        assert_eq!(c.suspend_limit, DEFAULT_SUSPEND_LIMIT);
        assert_eq!(c.webrtc, DEFAULT_WEBRTC);
        assert_eq!(c.signal_addr, DEFAULT_SIGNAL_ADDR);
        assert_eq!(c.signal_realm, DEFAULT_SIGNAL_REALM);
        assert_eq!(c.signal_skip_verify, DEFAULT_SIGNAL_SKIP_VERIFY);
        assert_eq!(c.ice_address, DEFAULT_ICE_ADDRESS);
        assert!(!c.no_service);
        assert!(!c.enable_fast_sync);
        assert!(!c.bootstrap);
        assert!(c.proxy.is_none());
        assert!(c.key.is_none());
    }

    #[test]
    fn set_data_dir_updates_default_database_dir() {
        let mut c = Config::new_default_config("a");
        // Pristine config has database_dir == default_database_dir().
        c.set_data_dir("/tmp/caspar-test");
        assert_eq!(c.data_dir, "/tmp/caspar-test");
        let expected = PathBuf::from("/tmp/caspar-test")
            .join(DEFAULT_DB_FILE)
            .to_string_lossy()
            .into_owned();
        assert_eq!(c.database_dir, expected);
    }

    #[test]
    fn set_data_dir_preserves_custom_database_dir() {
        let mut c = Config::new_default_config("a");
        c.database_dir = "/custom/db".to_string();
        c.set_data_dir("/tmp/something");
        // Custom database_dir is preserved.
        assert_eq!(c.database_dir, "/custom/db");
    }

    #[test]
    fn keyfile_and_certfile_are_under_data_dir() {
        let mut c = Config::new_default_config("a");
        c.data_dir = "/d".to_string();
        assert_eq!(c.keyfile(), "/d/priv_key");
        assert_eq!(c.cert_file(), "/d/cert.pem");
    }

    #[test]
    fn ice_servers_uses_credentials_from_config() {
        let mut c = Config::new_default_config("a");
        c.ice_address = "stun:host:3478".to_string();
        c.ice_username = "user".to_string();
        c.ice_password = "pass".to_string();
        let servers = c.ice_servers();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].urls, vec!["stun:host:3478".to_string()]);
        assert_eq!(servers[0].username, "user");
        assert_eq!(servers[0].credential, "pass");
        assert_eq!(servers[0].credential_type, "password");
    }

    #[test]
    fn default_ice_servers_contains_google_stun() {
        let servers = default_ice_servers();
        assert_eq!(servers.len(), 1);
        assert!(servers[0].urls.contains(&DEFAULT_ICE_ADDRESS.to_string()));
        assert!(servers[0].username.is_empty());
        assert!(servers[0].credential.is_empty());
    }

    #[test]
    fn log_level_maps_known_strings() {
        assert_eq!(log_level("debug"), Level::Debug);
        assert_eq!(log_level("info"), Level::Info);
        assert_eq!(log_level("warn"), Level::Warn);
        assert_eq!(log_level("error"), Level::Error);
        assert_eq!(log_level("fatal"), Level::Fatal);
        assert_eq!(log_level("panic"), Level::Panic);
    }

    #[test]
    fn log_level_defaults_to_debug_for_unknown() {
        assert_eq!(log_level(""), Level::Debug);
        assert_eq!(log_level("nope"), Level::Debug);
        assert_eq!(log_level("TRACE"), Level::Debug);
    }
}
