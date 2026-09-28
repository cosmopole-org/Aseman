//! ADR 0022 handoff: the legacy VM instances (`VmInstance::{program}::{entity}::{vm}`)
//! and external runtime handles, turned into workloads or stopped by a reviewed
//! operator decision. Legacy instance records are never treated as desired state:
//! nothing is adopted without an explicit decision, and nothing is dropped silently.
//!
//! The plan is read from a stopped node's store. Its digest is the approval token:
//! applying a decision set to a store whose plan changed is refused.

use super::*;
use serde::{Deserialize, Serialize};

/// One legacy VM instance record.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct LegacyVmInstance {
    pub program: String,
    pub entity: String,
    pub vm: String,
    /// The entity's recorded runtime (`vmEntityType`), empty when unrecorded.
    pub runtime: String,
    /// The Docker container the instance ran as, when recorded.
    pub container: Option<String>,
    pub started_at_millis: Option<i64>,
}

impl LegacyVmInstance {
    /// The instance's key in a decision set: `{program}::{entity}::{vm}`.
    #[must_use]
    pub fn key(&self) -> String {
        [
            self.program.as_str(),
            self.entity.as_str(),
            self.vm.as_str(),
        ]
        .join("::")
    }
}

/// A handle to a resource outside the node (a Modal app, image, volume, sandbox, or
/// a standalone Docker image or container).
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct LegacyExternalHandle {
    pub family: String,
    pub key: String,
    pub value: String,
}

/// What a stopped node's store holds, for review.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LegacyVmHandoffPlan {
    pub instances: Vec<LegacyVmInstance>,
    pub external: Vec<LegacyExternalHandle>,
    /// SHA-256 over the canonical plan: the approval token.
    pub digest: [u8; 32],
}

/// The operator's decision for one instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyVmDecision {
    /// Run it again as a workload of the node's VMM.
    Adopt,
    /// Do not run it again.
    Stop,
}

/// The operator's decisions: one per instance, and one per external handle.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LegacyVmHandoffDecisions {
    /// Keyed by [`LegacyVmInstance::key`].
    pub instances: BTreeMap<String, LegacyVmDecision>,
    /// External handles (`{family}::{key}`) the operator released outside Aseman.
    pub released: BTreeSet<String>,
    /// External handles (`{family}::{key}`) kept for their runtime module to adopt
    /// (P6-06; ADR 0022 for `ModalVolume` user data).
    pub kept: BTreeSet<String>,
}

const EXTERNAL_FAMILIES: [&str; 8] = [
    "ModalApp",
    "ModalImage",
    "ModalVolume",
    "ModalSandbox",
    "ModalProvisioning",
    "ModalProvisioningError",
    "vmStandaloneImageName",
    "vmStandaloneContainerName",
];

/// The instance-scoped observed links an applied handoff removes.
const INSTANCE_FAMILIES: [&str; 5] = [
    "VmInstance",
    "VmStatus",
    "VmStartedAt",
    "VmOwnerProgram",
    "vmDistributed",
];

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn link_rest<'a>(key: &'a [u8], family: &str) -> Option<&'a str> {
    std::str::from_utf8(key)
        .ok()?
        .strip_prefix("link::")?
        .strip_prefix(family)?
        .strip_prefix("::")
}

/// Read the handoff plan from a stopped node's store.
///
/// # Errors
///
/// A storage failure, or a malformed instance record.
pub fn plan_legacy_vm_handoff(
    store: &dyn LegacyKvStore,
) -> LegacyMigrationResult<LegacyVmHandoffPlan> {
    let mut instances = Vec::new();
    for (key, _) in store.scan_prefix(b"link::VmInstance::")? {
        let rest = link_rest(&key, "VmInstance").unwrap_or("");
        let parts: Vec<&str> = rest.split("::").collect();
        let [program, entity, vm] = parts.as_slice() else {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy VmInstance record {} is not program::entity::vm",
                text(&key)
            )));
        };
        let get = |key: String| -> LegacyMigrationResult<Option<String>> {
            Ok(store
                .get(key.as_bytes())?
                .map(|value| text(&value))
                .filter(|value| !value.is_empty()))
        };
        instances.push(LegacyVmInstance {
            program: (*program).to_owned(),
            entity: (*entity).to_owned(),
            vm: (*vm).to_owned(),
            runtime: get(format!("link::vmEntityType::{program}::{entity}"))?.unwrap_or_default(),
            container: get(format!("link::VmContainerName::{program}::{entity}::{vm}"))?,
            started_at_millis: get(format!("link::VmStartedAt::{vm}"))?
                .and_then(|value| value.parse().ok()),
        });
    }
    // A recorded container runs outside the node: it is a handle to decide too.
    let mut external: Vec<LegacyExternalHandle> = instances
        .iter()
        .filter_map(|instance| {
            Some(LegacyExternalHandle {
                family: "VmContainerName".to_owned(),
                key: instance.key(),
                value: instance.container.clone()?,
            })
        })
        .collect();
    for family in EXTERNAL_FAMILIES {
        for (key, value) in store.scan_prefix(format!("link::{family}::").as_bytes())? {
            external.push(LegacyExternalHandle {
                family: family.to_owned(),
                key: link_rest(&key, family).unwrap_or("").to_owned(),
                value: text(&value),
            });
        }
    }
    instances.sort();
    external.sort();
    let mut hasher = Sha256::new();
    hasher.update(b"aseman.legacy-vm-handoff.v1\n");
    for instance in &instances {
        for part in [
            instance.program.as_str(),
            &instance.entity,
            &instance.vm,
            &instance.runtime,
            instance.container.as_deref().unwrap_or(""),
        ] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        hasher.update(b"\n");
    }
    for handle in &external {
        for part in [handle.family.as_str(), &handle.key, &handle.value] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        hasher.update(b"\n");
    }
    Ok(LegacyVmHandoffPlan {
        instances,
        external,
        digest: hasher.finalize().into(),
    })
}

/// Check a decision set against the plan it was made for: every instance and every
/// external handle is decided exactly once, and nothing unknown is named.
///
/// # Errors
///
/// `Invalid` naming the first undecided, doubly decided, or unknown entry.
pub fn check_legacy_vm_decisions(
    plan: &LegacyVmHandoffPlan,
    decisions: &LegacyVmHandoffDecisions,
) -> LegacyMigrationResult<()> {
    let invalid = |message: String| Err(LegacyMigrationError::Invalid(message));
    let keys: BTreeSet<String> = plan.instances.iter().map(LegacyVmInstance::key).collect();
    for key in &keys {
        if !decisions.instances.contains_key(key) {
            return invalid(format!("the legacy VM instance {key} has no decision"));
        }
    }
    for key in decisions.instances.keys() {
        if !keys.contains(key) {
            return invalid(format!("{key} is not a legacy VM instance of this plan"));
        }
    }
    let handles: BTreeSet<String> = plan
        .external
        .iter()
        .map(|handle| format!("{}::{}", handle.family, handle.key))
        .collect();
    for handle in &handles {
        match (
            decisions.released.contains(handle),
            decisions.kept.contains(handle),
        ) {
            (true, false) | (false, true) => {}
            (false, false) => {
                return invalid(format!("the external handle {handle} has no decision"));
            }
            (true, true) => {
                return invalid(format!(
                    "the external handle {handle} is both released and kept"
                ));
            }
        }
    }
    for handle in decisions.released.iter().chain(&decisions.kept) {
        if !handles.contains(handle) {
            return invalid(format!("{handle} is not an external handle of this plan"));
        }
    }
    // A stopped instance's container must be released, never kept: nothing would
    // own it.
    for instance in &plan.instances {
        if let (Some(container), Some(LegacyVmDecision::Stop)) = (
            &instance.container,
            decisions.instances.get(&instance.key()),
        ) && !decisions
            .released
            .contains(&format!("VmContainerName::{}", instance.key()))
        {
            return invalid(format!(
                "stopping {} needs its container {container} released, not kept (VmContainerName::{})",
                instance.key(),
                instance.key()
            ));
        }
    }
    Ok(())
}

/// Remove the observed runtime records of every decided instance, and the handles
/// the operator released, in one atomic batch. Call only after every adopted
/// instance runs as a workload. Kept handles stay for their runtime module.
///
/// # Errors
///
/// `Invalid` when the store's plan no longer has `approved_digest`, or the decisions
/// do not cover the plan.
pub fn complete_legacy_vm_handoff(
    store: &dyn LegacyKvStore,
    approved_digest: [u8; 32],
    decisions: &LegacyVmHandoffDecisions,
) -> LegacyMigrationResult<usize> {
    let plan = plan_legacy_vm_handoff(store)?;
    if plan.digest != approved_digest {
        return Err(LegacyMigrationError::Invalid(
            "the legacy VM handoff plan changed since it was approved; plan again".to_owned(),
        ));
    }
    check_legacy_vm_decisions(&plan, decisions)?;
    let mut keys = BTreeSet::new();
    for instance in &plan.instances {
        let scoped = instance.key();
        keys.insert(format!("link::VmInstance::{scoped}"));
        keys.insert(format!("link::VmContainerName::{scoped}"));
        for family in &INSTANCE_FAMILIES[1..] {
            keys.insert(format!("link::{family}::{}", instance.vm));
        }
    }
    for handle in &decisions.released {
        keys.insert(format!("link::{handle}"));
    }
    let writes: Vec<LegacyKvWrite> = keys
        .into_iter()
        .map(|key| LegacyKvWrite::Delete {
            key: key.into_bytes(),
        })
        .collect();
    store.write_batch(&writes)?;
    Ok(writes.len())
}

#[cfg(test)]
mod tests;
