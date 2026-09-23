//! Stateful workload portability (A605, ADR 0011).
//!
//! A workload that moves takes its data with it, or it does not move. The rules here
//! decide which — before anything is quiesced, copied, or started — because the
//! failure mode this prevents is a stateful workload silently recreated as an empty
//! one somewhere else.
//!
//! Live stateful migration is not an Aseman guarantee. A move that crosses a worker,
//! a runtime, or a VMM provider is refused unless every attached volume has a
//! compatible declared path.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// What may be done with a volume's data when its workload moves.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortabilityTier {
    /// Recreation may discard the data. Never presented as migrated.
    Ephemeral,
    /// Movable only by a provider-declared snapshot or export contract, and only
    /// within that provider.
    ProviderLocal,
    /// Quiesce, snapshot, checksum, copy, restore, verify, then start.
    PortableOffline,
    /// The data stays in a separately managed storage provider; the target must pass
    /// its attach and fencing checks.
    SharedExternal,
}

/// A volume attached to a workload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Volume {
    pub name: String,
    pub tier: PortabilityTier,
    /// The provider that holds it, for example `nomad` or `native-legacy`.
    pub provider: String,
    /// The snapshot format its provider writes, when it writes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_format: Option<String>,
}

/// Where a workload is, or is going.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub worker: String,
    pub runtime: String,
    pub provider: String,
    /// The CPU architecture the workload runs on.
    pub architecture: String,
}

/// What a move requires, once it is known to be possible.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MovePlan {
    /// Nothing is attached that must survive: the workload is simply started on the
    /// target. Data in `ephemeral` volumes is gone, and the caller was told.
    Recreate,
    /// The provider's own snapshot and restore, within that provider.
    ProviderSnapshot,
    /// Quiesce the source, snapshot it, checksum it, copy it, restore it, verify it,
    /// and only then start the target. The source stays fenced until then.
    OfflineCopy,
    /// The data does not move; the target attaches it after passing the external
    /// provider's fencing checks.
    Reattach,
}

/// Whether a workload may move from `from` to `to`, and what that would take.
///
/// The plan is the strongest requirement any attached volume imposes: one
/// `portable_offline` volume among ephemeral ones still means a quiesced offline copy.
///
/// # Errors
///
/// The reason the move is refused. A refusal is the point: an operator is told what is
/// in the way rather than discovering afterwards that data did not come along.
pub fn plan_move(
    volumes: &[Volume],
    from: &Placement,
    to: &Placement,
) -> Result<MovePlan, PortabilityError> {
    if from.architecture != to.architecture {
        // Memory and disk images are not portable across architectures, and a guest
        // built for one will not run on the other.
        return Err(PortabilityError::ArchitectureMismatch);
    }
    let crosses_provider = from.provider != to.provider;
    let crosses_runtime = from.runtime != to.runtime;

    let mut plan = MovePlan::Recreate;
    for volume in volumes {
        let required = match volume.tier {
            PortabilityTier::Ephemeral => MovePlan::Recreate,
            PortabilityTier::ProviderLocal => {
                if crosses_provider {
                    // This is the case ADR 0011 exists for: a provider-local volume
                    // cannot leave its provider, and recreating the workload without
                    // it would present an empty volume as a migration.
                    return Err(PortabilityError::ProviderLocalVolume {
                        name: volume.name.clone(),
                    });
                }
                MovePlan::ProviderSnapshot
            }
            PortabilityTier::PortableOffline => {
                if crosses_runtime && volume.snapshot_format.is_none() {
                    return Err(PortabilityError::NoSnapshotFormat {
                        name: volume.name.clone(),
                    });
                }
                MovePlan::OfflineCopy
            }
            PortabilityTier::SharedExternal => MovePlan::Reattach,
        };
        plan = strongest(plan, required);
    }
    Ok(plan)
}

/// The more demanding of two plans. Ordering is by what must happen, not by name.
fn strongest(left: MovePlan, right: MovePlan) -> MovePlan {
    let rank = |plan: MovePlan| match plan {
        MovePlan::Recreate => 0,
        MovePlan::Reattach => 1,
        MovePlan::ProviderSnapshot => 2,
        MovePlan::OfflineCopy => 3,
    };
    if rank(left) >= rank(right) {
        left
    } else {
        right
    }
}

/// Whether recreating a workload where it is keeps every volume's data.
///
/// `shared_external` data lives elsewhere and survives. `ephemeral` data does not, but
/// its owner declared that it may be discarded, so recreation is still within what was
/// promised. A `provider_local` or `portable_offline` volume is neither: recreating it
/// would present an empty volume as the workload's, so the caller is told `false` and
/// must snapshot or refuse rather than find out afterwards.
#[must_use]
pub fn recreation_is_lossless(volumes: &[Volume]) -> bool {
    volumes.iter().all(|volume| {
        matches!(
            volume.tier,
            PortabilityTier::SharedExternal | PortabilityTier::Ephemeral
        )
    })
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PortabilityError {
    #[error("a workload does not move between CPU architectures")]
    ArchitectureMismatch,
    #[error("the volume {name} is provider-local and cannot leave its provider")]
    ProviderLocalVolume { name: String },
    #[error("the volume {name} declares no snapshot format, so it cannot cross runtimes")]
    NoSnapshotFormat { name: String },
}

#[cfg(test)]
mod tests;
