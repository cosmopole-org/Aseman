//! Resumable administrative operation journals (A902).
//!
//! These are pure state transitions. Drivers persist the journal after each step and
//! perform the actual database, filesystem, service-manager, or network work.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Administrative workflows that must survive interruption.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Upgrade,
    Backup,
    Restore,
    Doctor,
    SupportBundle,
}

/// A step name shared by the operation contract and its persisted journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStep {
    Preflight,
    VerifyArtifacts,
    QuiesceWrites,
    SnapshotStores,
    CaptureCatalog,
    HashArtifacts,
    SignManifest,
    ResumeWrites,
    VerifyManifest,
    PrepareTarget,
    RestoreStores,
    ApplyCatalog,
    VerifyIntegrity,
    DrainServices,
    ApplyUpgrade,
    MigrateSchema,
    StartServices,
    CheckConfiguration,
    CheckDependencies,
    CheckStorage,
    CheckRuntime,
    CheckSecurity,
    CollectDiagnostics,
    RedactSecrets,
    PackageBundle,
    Health,
}

impl OperationKind {
    /// The normative ordered plan. Completed steps are never repeated.
    #[must_use]
    pub const fn steps(self) -> &'static [OperationStep] {
        use OperationStep as S;
        match self {
            Self::Upgrade => &[
                S::Preflight,
                S::VerifyArtifacts,
                S::SnapshotStores,
                S::DrainServices,
                S::ApplyUpgrade,
                S::MigrateSchema,
                S::StartServices,
                S::Health,
            ],
            Self::Backup => &[
                S::Preflight,
                S::QuiesceWrites,
                S::SnapshotStores,
                S::CaptureCatalog,
                S::HashArtifacts,
                S::SignManifest,
                S::ResumeWrites,
                S::VerifyIntegrity,
            ],
            Self::Restore => &[
                S::Preflight,
                S::VerifyManifest,
                S::PrepareTarget,
                S::RestoreStores,
                S::ApplyCatalog,
                S::VerifyIntegrity,
                S::StartServices,
                S::Health,
            ],
            Self::Doctor => &[
                S::CheckConfiguration,
                S::CheckDependencies,
                S::CheckStorage,
                S::CheckRuntime,
                S::CheckSecurity,
                S::Health,
            ],
            Self::SupportBundle => &[
                S::CollectDiagnostics,
                S::RedactSecrets,
                S::PackageBundle,
                S::VerifyIntegrity,
            ],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationJournal {
    pub kind: OperationKind,
    #[serde(default)]
    pub completed: Vec<OperationStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<OperationFailure>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationFailure {
    pub step: OperationStep,
    pub reason: String,
}

impl OperationJournal {
    #[must_use]
    pub const fn new(kind: OperationKind) -> Self {
        Self {
            kind,
            completed: Vec::new(),
            failure: None,
        }
    }

    #[must_use]
    pub fn next(&self) -> Option<OperationStep> {
        self.kind
            .steps()
            .iter()
            .copied()
            .find(|step| !self.completed.contains(step))
    }

    #[must_use]
    pub fn complete(&self) -> bool {
        self.next().is_none()
    }

    /// Records success and advances exactly once.
    pub fn succeed(&mut self, step: OperationStep) -> Result<(), OperationJournalError> {
        self.require_next(step)?;
        self.completed.push(step);
        self.failure = None;
        Ok(())
    }

    /// Records a retryable failure without advancing the journal.
    pub fn fail(
        &mut self,
        step: OperationStep,
        reason: impl Into<String>,
    ) -> Result<(), OperationJournalError> {
        self.require_next(step)?;
        self.failure = Some(OperationFailure {
            step,
            reason: reason.into(),
        });
        Ok(())
    }

    fn require_next(&self, step: OperationStep) -> Result<(), OperationJournalError> {
        if self.completed.contains(&step) {
            return Err(OperationJournalError::AlreadyCompleted(step));
        }
        if self.next() != Some(step) {
            return Err(OperationJournalError::OutOfOrder {
                expected: self.next(),
                actual: step,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum OperationJournalError {
    #[error("operation step {0:?} has already completed")]
    AlreadyCompleted(OperationStep),
    #[error("operation step {actual:?} is out of order; expected {expected:?}")]
    OutOfOrder {
        expected: Option<OperationStep>,
        actual: OperationStep,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_operation_resumes_and_completed_steps_never_repeat() {
        for kind in [
            OperationKind::Upgrade,
            OperationKind::Backup,
            OperationKind::Restore,
            OperationKind::Doctor,
            OperationKind::SupportBundle,
        ] {
            let mut journal = OperationJournal::new(kind);
            for step in kind.steps() {
                assert_eq!(journal.next(), Some(*step));
                journal.succeed(*step).unwrap();
            }
            assert!(journal.complete());
            assert_eq!(
                journal.succeed(kind.steps()[0]),
                Err(OperationJournalError::AlreadyCompleted(kind.steps()[0]))
            );
        }
    }

    #[test]
    fn failure_is_retryable_and_cannot_skip_a_step() {
        let mut journal = OperationJournal::new(OperationKind::Restore);
        assert!(matches!(
            journal.succeed(OperationStep::RestoreStores),
            Err(OperationJournalError::OutOfOrder { .. })
        ));
        journal
            .fail(OperationStep::Preflight, "target is not empty")
            .unwrap();
        assert_eq!(journal.next(), Some(OperationStep::Preflight));
        journal.succeed(OperationStep::Preflight).unwrap();
        assert_eq!(journal.failure, None);
        assert_eq!(journal.next(), Some(OperationStep::VerifyManifest));
    }

    #[test]
    fn backup_resumes_writes_before_post_backup_verification() {
        let steps = OperationKind::Backup.steps();
        let resume = steps
            .iter()
            .position(|step| *step == OperationStep::ResumeWrites)
            .unwrap();
        let verify = steps
            .iter()
            .position(|step| *step == OperationStep::VerifyIntegrity)
            .unwrap();
        assert!(resume < verify);
    }
}
