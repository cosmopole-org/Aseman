//! Wire shapes of the program, entity, and workload operations.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// propagates the creature program to every node instance of this
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
    #[must_use]
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
pub struct MachineBuildsInput {
    #[serde(rename = "machineId", default)]
    pub machine_id: String,
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub count: i64,
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