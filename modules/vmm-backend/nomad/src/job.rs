//! A workload as a Nomad job (A601, `contracts/vmm/nomad/mapping.json`).
//!
//! The mapping is one-way and total: every field Nomad needs comes from the workload
//! record, and nothing the workload declares is dropped silently. What Nomad cannot
//! express — egress policy, pause — is refused here rather than approximated.

use std::collections::BTreeMap;

use aseman_domain::vmm::{PortProtocol, WorkloadRecord};
use aseman_domain::{DesiredWorkloadState, ObservedWorkloadState, WorkloadId};
use aseman_ports::{PortError, PortResult};
use serde_json::{Value, json};

/// The job ID of a workload. The UUID is the identity, so a workload can never place
/// twice and a register is idempotent.
#[must_use]
pub fn job_id(workload: WorkloadId) -> String {
    format!("aseman-{workload}")
}

/// The workload a job ID names, when it is one of ours.
#[must_use]
pub fn workload_of(job_id: &str) -> Option<WorkloadId> {
    job_id
        .strip_prefix("aseman-")
        .and_then(|rest| rest.parse::<aseman_domain::Uuid>().ok())
        .map(WorkloadId::from_uuid)
}

/// The group and task name. One workload is one task, so both are fixed.
pub const TASK: &str = "workload";

/// The network a workload's allocation is placed on.
///
/// Nomad's own `bridge` mode gives a workload unrestricted egress. A workload's
/// policy is deny-by-default (A406), so a backend running on plain bridge cannot
/// honour it and must say so rather than quietly granting the internet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkMode {
    /// Nomad's built-in bridge. Egress is unrestricted, so only a workload that asks
    /// for unrestricted egress may run here.
    Bridge,
    /// A CNI network the operator configured, named without the `cni/` prefix. The
    /// network is what enforces the egress policy; the backend refuses a policy the
    /// named network is not declared to deliver.
    Cni {
        name: String,
        /// Whether this network denies egress by default.
        denies_egress: bool,
    },
}

impl NetworkMode {
    /// The `Mode` string Nomad wants.
    fn mode(&self) -> String {
        match self {
            Self::Bridge => "bridge".to_owned(),
            Self::Cni { name, .. } => format!("cni/{name}"),
        }
    }

    fn denies_egress(&self) -> bool {
        match self {
            Self::Bridge => false,
            Self::Cni { denies_egress, .. } => *denies_egress,
        }
    }
}

/// How a runtime is executed on Nomad.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Execution {
    /// The workload's own image, run by the Docker driver.
    Image,
    /// A hardened runner image the operator declared for this runtime, which fetches
    /// the program artifact with the workload credential and executes it.
    Runner(String),
}

/// Aseman's job meta. The observation reads the generation back from here, never from
/// the request, so a stale allocation is never reported at the current generation.
fn meta(record: &WorkloadRecord) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("aseman.workload".to_owned(), record.id.to_string()),
        ("aseman.owner".to_owned(), record.owner.clone()),
        (
            "aseman.generation".to_owned(),
            record.desired.generation.get().to_string(),
        ),
        (
            "aseman.creature".to_owned(),
            record.labels.creature_id.to_string(),
        ),
        (
            "aseman.program".to_owned(),
            record.labels.program_id.to_string(),
        ),
        ("aseman.entity".to_owned(), record.labels.entity_id.clone()),
        ("aseman.runtime".to_owned(), record.spec.runtime.clone()),
        (
            "aseman.artifact".to_owned(),
            record.spec.artifact.digest.clone(),
        ),
    ])
}

/// `vcpu_millis` is thousandths of a core and Nomad wants MHz; one core is declared
/// as 1000 MHz, so the numbers are the same. Nomad's floor is 1.
fn cpu_mhz(vcpu_millis: u64) -> u64 {
    vcpu_millis.max(1)
}

/// Nomad refuses a task under 10 MB.
fn memory_mb(memory_mib: u64) -> u64 {
    memory_mib.max(10)
}

/// The environment a runner task needs to find its program and its node. The
/// credential is never here: it goes to the task's secrets directory.
fn runner_environment(record: &WorkloadRecord) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "ASEMAN_GUEST_API".to_owned(),
            record.spec.bootstrap.guest_api_url.clone(),
        ),
        ("ASEMAN_WORKLOAD".to_owned(), record.id.to_string()),
        ("ASEMAN_RUNTIME".to_owned(), record.spec.runtime.clone()),
        ("ASEMAN_ENTRY".to_owned(), record.spec.entry.clone()),
        (
            "ASEMAN_ARTIFACT_DIGEST".to_owned(),
            record.spec.artifact.digest.clone(),
        ),
    ])
}

/// The group's ports, one per declared ingress port, labelled by its name.
fn ports(record: &WorkloadRecord) -> Vec<Value> {
    record
        .spec
        .network
        .ingress
        .iter()
        .map(|port| json!({"Label": port.name, "To": i64::from(port.port)}))
        .collect()
}

/// The services a workload's HTTP ports are discoverable as.
fn services(record: &WorkloadRecord) -> Vec<Value> {
    record
        .spec
        .network
        .ingress
        .iter()
        .filter(|port| port.protocol == PortProtocol::Http)
        .map(|port| {
            json!({
                "Name": format!("aseman-{}-{}", record.id, port.name),
                "PortLabel": port.name,
                "Provider": "nomad",
                "Tags": ["aseman", record.spec.runtime.clone()],
            })
        })
        .collect()
}

/// The workload as a Nomad job at its desired generation.
///
/// `count` is 1 only while the workload is desired running: every other desired state
/// leaves the job registered with no allocation, so a restart keeps the workload's
/// identity and its history.
///
/// # Errors
///
/// [`PortError::Unsupported`] when the workload declares something this mapping
/// refuses to approximate.
pub fn job(
    record: &WorkloadRecord,
    execution: &Execution,
    namespace: &str,
    datacenters: &[String],
    network: &NetworkMode,
) -> PortResult<Value> {
    if record.desired.state == DesiredWorkloadState::Paused {
        // Nomad has no allocation-level pause, and stopping the group is not one:
        // it destroys the process. A pausing runtime does it through the agent.
        return Err(PortError::Unsupported("pause on the Nomad backend"));
    }
    // An empty allow list is "reach nothing", which only a network that denies
    // egress can deliver. Placing such a workload on an unrestricted network would
    // hand it the internet and report success (A406).
    let wants_denial = record.spec.network.egress_allow.is_empty();
    if wants_denial && !network.denies_egress() {
        return Err(PortError::Unsupported(
            "denied egress on this network: the backend needs a CNI network that enforces it",
        ));
    }
    if !wants_denial {
        // Per-destination allowances need a policy the network can be told about.
        // Nothing here can express them, so they are refused rather than widened to
        // "everything" or narrowed to "nothing".
        return Err(PortError::Unsupported(
            "selective egress allowances on the Nomad backend",
        ));
    }
    let count = i64::from(record.desired.state == DesiredWorkloadState::Running);
    let config = match execution {
        Execution::Image => json!({
            "image": record.spec.artifact.reference,
        }),
        Execution::Runner(image) => json!({
            "image": image,
        }),
    };
    let mut environment = runner_environment(record);
    environment.extend(record.spec.environment.clone());
    let group_network = json!({
        "Mode": network.mode(),
        "DynamicPorts": ports(record),
    });
    Ok(json!({
        "ID": job_id(record.id),
        "Name": job_id(record.id),
        "Namespace": namespace,
        "Type": "service",
        "Datacenters": datacenters,
        "Meta": meta(record),
        "TaskGroups": [{
            "Name": TASK,
            "Count": count,
            "Networks": [group_network],
            "Services": services(record),
            "EphemeralDisk": {
                "SizeMB": i64::try_from(record.spec.resources.disk_mib.unwrap_or(300))
                    .unwrap_or(300),
            },
            "RestartPolicy": {
                // A workload that keeps failing is the node's problem to see, not
                // something to hide behind endless restarts.
                "Attempts": 2,
                "Interval": 300_000_000_000_i64,
                "Delay": 5_000_000_000_i64,
                "Mode": "fail",
            },
            "Tasks": [{
                "Name": TASK,
                "Driver": "docker",
                "Config": config,
                "Env": environment,
                "Resources": {
                    "CPU": i64::try_from(cpu_mhz(record.spec.resources.vcpu_millis)).unwrap_or(100),
                    "MemoryMB": i64::try_from(memory_mb(record.spec.resources.memory_mib))
                        .unwrap_or(128),
                },
                "LogConfig": {
                    "MaxFiles": 2,
                    "MaxFileSizeMB": 10,
                },
            }],
        }],
    }))
}

/// What Nomad's allocation status says the workload is doing.
#[must_use]
pub fn observed_state(client_status: &str) -> ObservedWorkloadState {
    match client_status {
        "pending" => ObservedWorkloadState::Pending,
        "running" => ObservedWorkloadState::Running,
        "complete" => ObservedWorkloadState::Stopped,
        "failed" => ObservedWorkloadState::Failed,
        "lost" => ObservedWorkloadState::Lost,
        _ => ObservedWorkloadState::Unknown,
    }
}

#[cfg(test)]
mod tests;
