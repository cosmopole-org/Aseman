//! ADR 0022: legacy VM state. Observed runtime is shape-checked, inventoried for the
//! native-legacy VMM backend, and never exported; durable intent migrates elsewhere.

use super::*;

/// Expected value shape of an observed-runtime link.
#[derive(Clone, Copy, Debug)]
enum ObservedValue {
    Literal(&'static str),
    Digits,
    NonEmpty,
}

/// `(family, identity segments, value shape)` for every observed-runtime link family.
const OBSERVED_LINKS: [(&str, usize, ObservedValue); 17] = [
    ("VmInstance", 3, ObservedValue::Literal("true")),
    ("VmStatus", 1, ObservedValue::Literal("running")),
    ("VmStartedAt", 1, ObservedValue::Digits),
    ("VmOwnerProgram", 1, ObservedValue::NonEmpty),
    ("vmDistributed", 1, ObservedValue::Literal("true")),
    ("VmContainerName", 3, ObservedValue::NonEmpty),
    ("vmStandaloneImageName", 2, ObservedValue::NonEmpty),
    ("vmStandaloneContainerName", 2, ObservedValue::NonEmpty),
    ("VmTerminal", 3, ObservedValue::Literal("true")),
    ("VmBuilds", 2, ObservedValue::Literal("true")),
    ("ProxyCorrExpiry", 1, ObservedValue::Digits),
    ("ModalApp", 1, ObservedValue::NonEmpty),
    ("ModalImage", 2, ObservedValue::NonEmpty),
    ("ModalVolume", 1, ObservedValue::NonEmpty),
    ("ModalSandbox", 1, ObservedValue::NonEmpty),
    ("ModalProvisioning", 1, ObservedValue::NonEmpty),
    ("ModalProvisioningError", 1, ObservedValue::NonEmpty),
];

pub const LEGACY_PROXY_CORRELATION_PREFIX: &str = "Json::ProxyCorrelation::";

/// `true` for a link family that ADR 0022 classifies as VMM-owned observed runtime.
#[must_use]
pub fn is_legacy_observed_vm_link_family(family: &str) -> bool {
    OBSERVED_LINKS.iter().any(|(name, _, _)| *name == family)
}

/// `true` for a link family ADR 0022 removes or verifies without exporting.
#[must_use]
pub fn is_legacy_vm_control_link_family(family: &str) -> bool {
    matches!(family, "vmDistribution" | "VmBilling")
}

impl LegacySnapshotGraph {
    /// Verify observed runtime and control links; return the VMM handoff inventory
    /// (per-family record counts). Nothing here becomes a capsule.
    pub fn legacy_vmm_handoff_inventory(&self) -> LegacyMigrationResult<BTreeMap<String, usize>> {
        let mut inventory = BTreeMap::new();
        for (key, value) in &self.links {
            let Some((family, rest)) = key.split_once("::") else {
                continue;
            };
            let Some((_, segments, shape)) =
                OBSERVED_LINKS.iter().find(|(name, _, _)| *name == family)
            else {
                continue;
            };
            let text = std::str::from_utf8(value).unwrap_or("");
            let identity_ok = rest.split("::").count() == *segments
                && rest.split("::").all(|segment| !segment.is_empty());
            let value_ok = match shape {
                ObservedValue::Literal(expected) => text == *expected,
                ObservedValue::Digits => {
                    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
                }
                ObservedValue::NonEmpty => !text.is_empty() && !value.is_empty(),
            };
            if !identity_ok || !value_ok {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy observed runtime link {key} does not match the reviewed {family} shape"
                )));
            }
            *inventory.entry(family.to_owned()).or_insert(0) += 1;
        }
        for (key, records) in &self.documents {
            if key.starts_with(LEGACY_PROXY_CORRELATION_PREFIX) {
                verified_legacy_document(key, "record", records)?;
                *inventory
                    .entry("Json::ProxyCorrelation".to_owned())
                    .or_insert(0) += 1;
            }
        }
        Ok(inventory)
    }

    /// `vmDistribution` is OpenRaft replication scope removed by ADR 0012; the
    /// `VmBilling` flag must match a finance payment record exactly (ADR 0017).
    pub(crate) fn verify_legacy_vm_control_links(&self) -> LegacyMigrationResult<()> {
        let payments: BTreeSet<&str> = self
            .documents
            .keys()
            .filter_map(|key| key.strip_prefix("Json::VmBilling::"))
            .collect();
        let mut flagged = BTreeSet::new();
        for (key, value) in &self.links {
            if let Some(rest) = key.strip_prefix("vmDistribution::") {
                let segments = rest.split("::").collect::<Vec<_>>();
                if !(1..=2).contains(&segments.len())
                    || segments.iter().any(|segment| segment.is_empty())
                    || !matches!(value.as_slice(), b"cluster" | b"local")
                {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "legacy {key} is not a reviewed cluster/local distribution scope"
                    )));
                }
            } else if let Some(vm) = key.strip_prefix("VmBilling::") {
                if value != b"true" || !payments.contains(vm) {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "legacy VmBilling flag {vm} has no matching payment record"
                    )));
                }
                flagged.insert(vm);
            }
        }
        if let Some(vm) = payments.difference(&flagged).next() {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy VmBilling payment {vm} has no billing-sweep flag"
            )));
        }
        Ok(())
    }
}
