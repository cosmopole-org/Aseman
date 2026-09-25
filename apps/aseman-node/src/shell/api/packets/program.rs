//! Request and response payloads for the `program` action namespace —
//! the program / app / machine lifecycle actions exposed by the shell.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::input::IInput;

macro_rules! input_impls {
    ($($t:ty => $origin:expr),* $(,)?) => {
        $(
            impl IInput for $t {
                fn get_store_id(&self) -> String { String::new() }
                fn origin(&self) -> String { $origin.to_string() }
                fn as_any(&self) -> &dyn std::any::Any { self }
            }
        )*
    };
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListAppMachsInput {
    #[serde(rename = "appId", default)]
    pub app_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadVmLogsInput {
    #[serde(rename = "vmId", default)]
    pub vm_id: String,
    #[serde(rename = "logType", default)]
    pub log_type: String,
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub count: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateAppInput {
    #[serde(rename = "chainId", default)]
    pub chain_id: String,
    #[serde(
        rename = "shardChainId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub shard_chain_id: Option<String>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateMachineInput {
    #[serde(default)]
    pub username: String,
    #[serde(rename = "appId", default)]
    pub app_id: String,
    #[serde(default)]
    pub path: String,
    #[serde(rename = "Comment", default)]
    pub comment: String,
    #[serde(default)]
    pub runtime: String,
    #[serde(rename = "publicKey", default)]
    pub public_key: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeleteAppInput {
    #[serde(rename = "appId", default)]
    pub app_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeleteProgramInput {
    #[serde(rename = "programId", default)]
    pub program_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeployInput {
    #[serde(rename = "machineId", default)]
    pub machine_id: String,
    #[serde(rename = "entityId", default)]
    pub entity_id: String,
    #[serde(rename = "entityType", default)]
    pub entity_type: String,
    #[serde(default)]
    pub downloadable: bool,
    #[serde(default)]
    pub payload: String,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
    /// Deployment scope: `"cluster"` (aliases: `"global"`, `"distributed"`)
    /// propagates the creature program to every Caspar instance of this
    /// origin so any edge instance can execute it; `"local"` (default, alias
    /// empty) keeps it on the receiving instance only.
    #[serde(default)]
    pub distribution: String,
    /// Boolean shorthand for `distribution: "cluster"`.
    #[serde(default)]
    pub distributed: bool,
}

impl DeployInput {
    /// True when the developer opted into cluster-wide propagation.
    pub fn wants_distribution(&self) -> bool {
        self.distributed
            || matches!(
                self.distribution.trim().to_lowercase().as_str(),
                "cluster" | "global" | "distributed"
            )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListInput {
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub count: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateAppInput {
    #[serde(rename = "appId", default)]
    pub app_id: String,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MachineBuildsInput {
    #[serde(rename = "machineId", default)]
    pub machine_id: String,
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub count: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SignalInput {
    #[serde(default)]
    pub data: String,
    #[serde(rename = "machineId", default)]
    pub machine_id: String,
    #[serde(rename = "vmTag", default)]
    pub vm_tag: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateProgramInput {
    #[serde(rename = "programId", default)]
    pub program_id: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VmTerminalInput {
    #[serde(rename = "creatureId", default)]
    pub creature_id: String,
    #[serde(rename = "vmId", default)]
    pub vm_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VmResourcesInput {
    #[serde(rename = "maxExecTimeSeconds", default)]
    pub max_exec_time_seconds: i64,
    #[serde(rename = "ramMb", default)]
    pub ram_mb: i64,
    #[serde(rename = "diskGb", default)]
    pub disk_gb: i64,
    #[serde(rename = "cpuCores", default)]
    pub cpu_cores: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunProgramEntityInput {
    #[serde(rename = "programId", default)]
    pub program_id: String,
    #[serde(rename = "machineId", default)]
    pub machine_id: String,
    #[serde(rename = "entityId", default)]
    pub entity_id: String,
    #[serde(rename = "vmId", default)]
    pub vm_id: String,
    #[serde(default)]
    pub resources: VmResourcesInput,
    #[serde(default)]
    pub params: HashMap<String, String>,
    #[serde(
        rename = "paymentLockId",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub payment_lock_id: String,
    #[serde(
        rename = "paymentSignatures",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub payment_signatures: Vec<String>,
    /// Optional custom VM gateway route to bind to this launched instance. When
    /// set, the entity's HTTP server becomes reachable at the deterministic
    /// `/{creatureUsername}/{gatewayPath…}` ingress form, pointing at the exact
    /// instance this call starts — so a standalone serving tool (e.g. the github
    /// OAuth callback) has a fixed URL that survives redeploys (the route is
    /// re-pointed at the fresh instance each run).
    #[serde(rename = "gatewayPath", default)]
    pub gateway_path: String,
}

/// `/programs/downloadEntity` — fetch a downloadable entity's script/file so
/// a client can execute it locally (e.g. a deployed front-end app).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DownloadEntityInput {
    #[serde(rename = "machineId", default)]
    pub machine_id: String,
    #[serde(rename = "programId", default)]
    pub program_id: String,
    #[serde(rename = "entityId", default)]
    pub entity_id: String,
}

input_impls! {
    DownloadEntityInput => "",
    ListAppMachsInput => "",
    ReadVmLogsInput => "global",
    CreateAppInput => "global",
    CreateMachineInput => "global",
    DeleteAppInput => "global",
    DeleteProgramInput => "global",
    DeployInput => "global",
    ListInput => "",
    UpdateAppInput => "global",
    MachineBuildsInput => "global",
    SignalInput => "",
    UpdateProgramInput => "global",
    VmTerminalInput => "global",
    // VM lifecycle (run / stop / list instances — all `RunProgramEntityInput`)
    // is per-node routing, not global state. Origin "" routes it through the
    // local execution path (`modify_state_securly` → direct storage commit /
    // raft in cluster mode), and federation to the owning node when remote —
    // instead of a babble consensus round per invocation. The creature's own
    // execution writes already take the local path (`app.modify_state` in the
    // vmm host calls); only this invocation wrapper was needlessly on-chain.
    // Token charging stays on-chain via `charge_running_standalone_vms`.
    RunProgramEntityInput => "",
}
