//! The bootstrap workflow (Phase 9, plan 08).
//!
//! One workflow takes a clean host to a healthy node. It is idempotent, it resumes
//! after an interruption, and a failed stage rolls itself back without destroying
//! working data.
//!
//! The rule that shapes everything here: **a stage that has already succeeded is never
//! run again.** An installer that re-runs its own steps is an installer that can
//! destroy a working deployment on its second invocation, which is exactly what the
//! legacy imperative script could do.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The stages, in the order they run. Each one depends on every stage before it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Check the host: OS, architecture, cgroups, KVM, ports, DNS, time, disk, memory.
    Preflight,
    /// Choose compact or clustered topology.
    Topology,
    /// Pin and verify artifact versions and checksums.
    Artifacts,
    /// Generate keys, certificates, secrets, and typed configuration.
    Identity,
    /// Apply database schemas and provider configuration.
    Schema,
    /// Start dependencies in order and wait for readiness.
    Services,
    /// Run end-to-end health checks.
    Health,
}

impl Stage {
    /// Every stage, in order.
    pub const ALL: [Self; 7] = [
        Self::Preflight,
        Self::Topology,
        Self::Artifacts,
        Self::Identity,
        Self::Schema,
        Self::Services,
        Self::Health,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Topology => "topology",
            Self::Artifacts => "artifacts",
            Self::Identity => "identity",
            Self::Schema => "schema",
            Self::Services => "services",
            Self::Health => "health",
        }
    }

    /// Whether rolling this stage back can destroy data an operator would want.
    ///
    /// The schema stage is the line: before it, a rollback removes files the bootstrap
    /// itself created. At and after it, a rollback that "cleaned up" would drop a
    /// database. So those stages are rolled *forward* by re-running them, never undone.
    #[must_use]
    pub const fn rollback_is_destructive(self) -> bool {
        matches!(self, Self::Schema | Self::Services | Self::Health)
    }
}

/// How a stage ended.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum StageOutcome {
    /// Done. It will not run again.
    Done,
    /// It failed, with a reason an operator can act on.
    Failed { reason: String },
}

/// What the bootstrap has done so far, as it is written to disk after every stage.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    /// Stages that have succeeded, in order.
    #[serde(default)]
    pub done: Vec<Stage>,
    /// The stage that failed, if the last attempt failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<(Stage, String)>,
}

impl Progress {
    /// Whether `stage` has already succeeded.
    #[must_use]
    pub fn is_done(&self, stage: Stage) -> bool {
        self.done.contains(&stage)
    }

    /// The next stage to run, or `None` when the node is up.
    ///
    /// Resuming is just this: ask what is next. An interrupted bootstrap re-entered
    /// from the beginning skips everything it already did.
    #[must_use]
    pub fn next(&self) -> Option<Stage> {
        Stage::ALL.into_iter().find(|stage| !self.is_done(*stage))
    }

    /// Record a stage's outcome.
    ///
    /// # Errors
    ///
    /// [`BootstrapError::OutOfOrder`] when a stage is recorded before the one it
    /// depends on, and [`BootstrapError::AlreadyDone`] when a finished stage is
    /// recorded again — an installer that re-runs a finished stage is one that can
    /// destroy a working deployment.
    pub fn record(&mut self, stage: Stage, outcome: &StageOutcome) -> Result<(), BootstrapError> {
        if self.is_done(stage) {
            return Err(BootstrapError::AlreadyDone(stage));
        }
        if self.next() != Some(stage) {
            return Err(BootstrapError::OutOfOrder(stage));
        }
        match outcome {
            StageOutcome::Done => {
                self.done.push(stage);
                self.failed = None;
            }
            StageOutcome::Failed { reason } => {
                self.failed = Some((stage, reason.clone()));
            }
        }
        Ok(())
    }

    /// Whether the node is up.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.next().is_none()
    }
}

/// What rolling back a failed stage means.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rollback {
    /// Undo what the stage created. Safe: it created it.
    Undo,
    /// Do not undo. Re-run the stage instead; undoing would destroy working data.
    RollForward,
}

/// How to recover from a failed stage.
#[must_use]
pub fn rollback(stage: Stage) -> Rollback {
    if stage.rollback_is_destructive() {
        Rollback::RollForward
    } else {
        Rollback::Undo
    }
}

/// One preflight finding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub check: String,
    /// Whether the host may proceed despite it.
    pub fatal: bool,
    pub detail: String,
}

/// Whether preflight passed: no fatal finding.
///
/// A non-fatal finding is reported and the bootstrap continues. A host without KVM can
/// run containers perfectly well; refusing to install because it cannot also run
/// microVMs would be wrong.
#[must_use]
pub fn preflight_passed(findings: &[Finding]) -> bool {
    !findings.iter().any(|finding| finding.fatal)
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BootstrapError {
    #[error("the stage {0:?} has already succeeded and must not run again")]
    AlreadyDone(Stage),
    #[error("the stage {0:?} cannot run before the ones it depends on")]
    OutOfOrder(Stage),
}

#[cfg(test)]
mod tests;
