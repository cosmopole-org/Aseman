//! Replicated command set of the Caspar geo-distributed cluster.
//!
//! Every state change that must be visible on all Caspar instances of the
//! same authority travels through the OpenRaft log as a [`ClusterCommand`].
//! Three producers feed the log:
//!
//! * **Shell API write-sets** — every non-readonly shell action commit is
//!   captured as a [`ClusterCommand::KvBatch`] so the resulting state is
//!   available on every instance (accounts, machines, programs, stores, …).
//! * **Distributed creature deployments** — a `/programs/deploy` call with
//!   `distribution: "cluster"` emits a [`ClusterCommand::Deploy`] carrying the
//!   creature artifact itself, so every instance can serve the VM locally.
//! * **Distributed VM state** — key-value mutations made by VMs that were
//!   deployed in distributed mode commit locally first (edge-style, on the
//!   instance that received the request) and are then propagated as
//!   [`ClusterCommand::KvBatch`]. VMs deployed in local mode never enter the
//!   raft log.

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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeployArtifact {
    pub program_id: String,
    pub entity_id: String,
    pub entity_type: String,
    /// Owning Machine of the program (ownership resolution on replicas).
    pub machine_id: String,
    pub runtime: String,
    pub path: String,
    pub comment: String,
    /// Primary artifact file name declared by the runtime plugin.
    pub primary_file_name: String,
    /// `(file_name, base64_data)` — primary file first, extras after.
    pub files: Vec<(String, String)>,
    pub set_entity_links: bool,
    pub build_on_deploy: bool,
    /// Normalized custom VM gateway route prefix bound to this entity, or empty
    /// when it exposes no custom route. Reached at `/{creatureUsername}/{path…}`
    /// where the username belongs to `machine_id` (the owning creature).
    #[serde(default)]
    pub gateway_route: String,
    /// Optional specific VM instance the custom route targets (empty ⇒ default).
    #[serde(default)]
    pub gateway_vm_id: String,
}

/// The application data payload of a raft log entry.
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
    #[serde(rename = "deploy")]
    Deploy {
        origin: u64,
        artifact: DeployArtifact,
    },
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
    /// Raft type bundle of the Caspar cluster: `D` is the replicated command,
    /// `R` the apply result; node ids are `u64` and nodes are addressed
    /// `openraft::BasicNode`s (host:port of the cluster HTTP listener).
    pub TypeConfig:
        D = ClusterCommand,
        R = ClusterResponse,
);
