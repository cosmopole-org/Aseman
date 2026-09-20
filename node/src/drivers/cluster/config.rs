//! Cluster configuration — persisted, env-seeded, and fully editable through
//! `casparctl cluster config …`.
//!
//! The config lives as JSON at `<storage_root>/cluster/cluster.json` (override
//! with `CLUSTER_CONFIG_PATH`). Every field is addressable by a dotted key
//! (`heartbeat_interval_ms`, `peers.2.addr`, `extra.myKnob`) so operators can
//! modify each knob individually from the CLI, or replace the whole file at
//! once with `casparctl cluster apply -f cluster.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use aseman_config::ClusterBootstrapConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A remote Caspar instance of the same origin (authority).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PeerConfig {
    /// Raft node id — unique across the cluster.
    pub id: u64,
    /// `host:port` (domain or IP) of the peer's cluster listener.
    pub addr: String,
    /// Free-form geographic region tag (e.g. "eu-west", "ap-south").
    #[serde(default)]
    pub region: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    /// Whether this peer should be a raft voter (true) or a learner /
    /// read-replica edge (false).
    #[serde(default = "default_true")]
    pub voter: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClusterConfig {
    /// Master switch. When false the node runs standalone exactly as before.
    pub enabled: bool,
    /// Seed flag: exactly one instance of a brand-new cluster starts with
    /// `bootstrap: true` — it initializes itself as the first voter and
    /// accepts the others via `casparctl cluster add-peer`. Joining
    /// instances MUST leave this false (they stay pristine until the seed
    /// adds them); `casparctl cluster init` can initialize manually instead.
    pub bootstrap: bool,
    /// This node's raft id.
    pub node_id: u64,
    /// Human-readable name shown in `casparctl cluster status`.
    pub node_name: String,
    /// Geographic region of this instance.
    pub region: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    /// Bind address of the cluster HTTP listener (raft RPC + admin API).
    pub listen_addr: String,
    /// Address other instances use to reach this node (domain or IP).
    pub advertise_addr: String,
    /// Optional shared secret; when set every cluster HTTP call must carry
    /// it in the `x-caspar-cluster-token` header.
    pub auth_token: String,

    // ── Raft tuning ─────────────────────────────────────────────────────
    pub heartbeat_interval_ms: u64,
    pub election_timeout_min_ms: u64,
    pub election_timeout_max_ms: u64,
    pub max_payload_entries: u64,
    /// Take a snapshot every N log entries.
    pub snapshot_logs_since_last: u64,
    pub max_in_snapshot_log_to_keep: u64,

    // ── Edge / geo routing ──────────────────────────────────────────────
    /// Seconds between RTT probes towards every known peer.
    pub rtt_probe_interval_secs: u64,
    /// Max buffered replication proposals before backpressure logging.
    pub proposal_queue_capacity: usize,

    /// Known peers of this cluster, keyed by node id (stringified in JSON).
    pub peers: BTreeMap<u64, PeerConfig>,

    /// Free-form operator knobs, individually settable via the CLI and
    /// replicated cluster-wide through `ClusterCommand::ConfigPut`.
    pub extra: BTreeMap<String, Value>,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        ClusterConfig {
            enabled: false,
            bootstrap: false,
            node_id: 1,
            node_name: String::new(),
            region: String::new(),
            latitude: None,
            longitude: None,
            listen_addr: "0.0.0.0:7440".to_string(),
            advertise_addr: "127.0.0.1:7440".to_string(),
            auth_token: String::new(),
            heartbeat_interval_ms: 500,
            election_timeout_min_ms: 1500,
            election_timeout_max_ms: 3000,
            max_payload_entries: 300,
            snapshot_logs_since_last: 5000,
            max_in_snapshot_log_to_keep: 1000,
            rtt_probe_interval_secs: 15,
            proposal_queue_capacity: 16384,
            peers: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }
}

impl ClusterConfig {
    /// Resolve the config path: `CLUSTER_CONFIG_PATH` env override or
    /// `<storage_root>/cluster/cluster.json`.
    pub fn default_path(storage_root: &str, source: &ClusterBootstrapConfig) -> PathBuf {
        if let Some(path) = &source.config_path {
            return PathBuf::from(path);
        }
        Path::new(storage_root).join("cluster").join("cluster.json")
    }

    pub fn load(path: &Path) -> Result<ClusterConfig> {
        let raw = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// Load from disk (if present) and fold typed composition overrides on top.
    pub fn bootstrap(
        storage_root: &str,
        source: &ClusterBootstrapConfig,
    ) -> (ClusterConfig, PathBuf) {
        let path = Self::default_path(storage_root, source);
        let mut cfg = Self::load(&path).unwrap_or_default();
        if let Some(value) = source.enabled {
            cfg.enabled = value;
        }
        if let Some(value) = source.bootstrap {
            cfg.bootstrap = value;
        }
        if let Some(value) = source.node_id {
            cfg.node_id = value;
        }
        let apply = |value: &Option<String>, slot: &mut String| {
            if let Some(value) = value {
                *slot = value.clone();
            }
        };
        apply(&source.node_name, &mut cfg.node_name);
        apply(&source.region, &mut cfg.region);
        apply(&source.listen_addr, &mut cfg.listen_addr);
        apply(&source.advertise_addr, &mut cfg.advertise_addr);
        apply(&source.auth_token, &mut cfg.auth_token);
        if cfg.node_name.is_empty() {
            cfg.node_name = format!("caspar-node-{}", cfg.node_id);
        }
        (cfg, path)
    }

    /// Read a single dotted-key value (`peers.2.addr`, `heartbeat_interval_ms`).
    pub fn get_key(&self, key: &str) -> Option<Value> {
        let root = serde_json::to_value(self).ok()?;
        let pointer = format!("/{}", key.replace('.', "/"));
        root.pointer(&pointer).cloned()
    }

    /// Set a single dotted-key value; the string is parsed as JSON when
    /// possible, otherwise stored as a bare string. Returns the updated
    /// config or an error when the result no longer deserializes.
    pub fn set_key(&self, key: &str, raw_value: &str) -> Result<ClusterConfig> {
        let value: Value = serde_json::from_str(raw_value)
            .unwrap_or_else(|_| Value::String(raw_value.to_string()));
        let mut root = serde_json::to_value(self)?;
        let parts: Vec<&str> = key.split('.').collect();
        if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
            return Err(anyhow!("invalid config key: {}", key));
        }
        let mut cursor = &mut root;
        for part in &parts[..parts.len() - 1] {
            cursor = match cursor {
                Value::Object(map) => map
                    .entry(part.to_string())
                    .or_insert_with(|| Value::Object(Default::default())),
                _ => return Err(anyhow!("config key {} traverses a non-object", key)),
            };
        }
        match cursor {
            Value::Object(map) => {
                map.insert(parts[parts.len() - 1].to_string(), value);
            }
            _ => return Err(anyhow!("config key {} traverses a non-object", key)),
        }
        serde_json::from_value(root).map_err(|e| anyhow!("invalid value for {}: {}", key, e))
    }

    /// OpenRaft config derived from the tunables.
    pub fn raft_config(&self) -> openraft::Config {
        openraft::Config {
            cluster_name: "caspar-cluster".to_string(),
            heartbeat_interval: self.heartbeat_interval_ms,
            election_timeout_min: self.election_timeout_min_ms,
            election_timeout_max: self.election_timeout_max_ms,
            max_payload_entries: self.max_payload_entries,
            snapshot_policy: openraft::SnapshotPolicy::LogsSinceLast(self.snapshot_logs_since_last),
            max_in_snapshot_log_to_keep: self.max_in_snapshot_log_to_keep,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_scalar_key() {
        let cfg = ClusterConfig::default();
        let updated = cfg.set_key("heartbeat_interval_ms", "250").unwrap();
        assert_eq!(updated.heartbeat_interval_ms, 250);
        assert_eq!(
            updated.get_key("heartbeat_interval_ms"),
            Some(serde_json::json!(250))
        );
    }

    #[test]
    fn set_nested_peer_key() {
        let mut cfg = ClusterConfig::default();
        cfg.peers.insert(
            2,
            PeerConfig {
                id: 2,
                addr: "old:7440".into(),
                ..Default::default()
            },
        );
        let updated = cfg
            .set_key("peers.2.addr", "caspar-us.example.com:7440")
            .unwrap();
        assert_eq!(updated.peers[&2].addr, "caspar-us.example.com:7440");
    }

    #[test]
    fn set_free_form_extra_key() {
        let cfg = ClusterConfig::default();
        let updated = cfg
            .set_key("extra.maintenanceWindow", "\"02:00-04:00Z\"")
            .unwrap();
        assert_eq!(
            updated.extra.get("maintenanceWindow"),
            Some(&serde_json::json!("02:00-04:00Z"))
        );
    }

    #[test]
    fn invalid_typed_value_is_rejected() {
        let cfg = ClusterConfig::default();
        assert!(cfg.set_key("node_id", "\"not-a-number\"").is_err());
    }

    #[test]
    fn bare_strings_are_accepted_without_quotes() {
        let cfg = ClusterConfig::default();
        let updated = cfg.set_key("region", "eu-west").unwrap();
        assert_eq!(updated.region, "eu-west");
    }
}
