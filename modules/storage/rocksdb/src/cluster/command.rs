//! The replicated command set of the RocksDB provider's cluster (ADR 0033).
//!
//! Every write batch of the replicated store travels through the OpenRaft log as a
//! [`ClusterCommand::KvBatch`] and is applied, in log order, to every replica's
//! RocksDB. Cluster configuration changes travel as [`ClusterCommand::ConfigPut`]. The
//! log carries storage only: nothing above the storage provider proposes to it.

use std::io::Cursor;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One key/value mutation inside a replicated write-set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum KvOp {
    #[serde(rename = "put")]
    Put {
        key: String,
        /// Base64 of the raw value bytes (values are arbitrary binary).
        value_b64: String,
    },
    #[serde(rename = "del")]
    Del { key: String },
}

impl KvOp {
    pub fn put(key: String, value: &[u8]) -> KvOp {
        KvOp::Put {
            key,
            value_b64: B64.encode(value),
        }
    }

    pub fn del(key: String) -> KvOp {
        KvOp::Del { key }
    }

    pub fn decoded_value(&self) -> Vec<u8> {
        match self {
            KvOp::Put { value_b64, .. } => B64.decode(value_b64).unwrap_or_default(),
            KvOp::Del { .. } => Vec::new(),
        }
    }
}

/// A creature program artifact replicated to every instance on a
/// distributed deploy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClusterCommand {
    /// No-op used for leader-commit probes.
    #[serde(rename = "noop")]
    Noop,
    /// Replicated key-value write-set (shell actions / distributed VM state).
    #[serde(rename = "kv")]
    KvBatch {
        /// Raft node id of the instance the batch was executed on. The origin
        /// already applied the batch locally, so it skips re-application.
        origin: u64,
        ops: Vec<KvOp>,
    },
    /// Replicated creature program deployment.
    /// Cluster-wide configuration entry (kept in the replicated config store).
    #[serde(rename = "config")]
    ConfigPut { key: String, value: Value },
}

/// The state machine's answer for one applied command.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClusterResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
}

impl ClusterResponse {
    pub fn ok() -> Self {
        ClusterResponse {
            ok: true,
            err: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        ClusterResponse {
            ok: false,
            err: Some(msg.into()),
        }
    }
}

openraft::declare_raft_types!(
    /// Raft type bundle of the storage cluster: `D` is the replicated command,
    /// `R` the apply result; node ids are `u64` and nodes are addressed
    /// `openraft::BasicNode`s (host:port of the cluster HTTP listener).
    pub TypeConfig:
        D = ClusterCommand,
        R = ClusterResponse,
);
