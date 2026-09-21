//! Provider migration protocol (A309): a pure state machine for the plan's ten steps
//! plus a provider-neutral semantic comparison. No storage, clock, or I/O lives here.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Canonical identity of one migrated record: capsule kind and 128-bit capsule ID.
pub type RecordKey = (String, [u8; 16]);

/// A provider-neutral view of one canonical capsule used for comparison.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationRecord {
    pub kind: String,
    pub id: [u8; 16],
    /// Semantic digest of the capsule (kind, identity, owner, relationships, body);
    /// it excludes the revision chain so re-applied revisions compare equal.
    pub digest: [u8; 32],
    pub tombstone: bool,
    /// Relationship targets that must exist in the same record set.
    pub references: Vec<RecordKey>,
}

/// Semantic comparison of a source and target record set.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ComparisonReport {
    pub source_counts: BTreeMap<String, u64>,
    pub target_counts: BTreeMap<String, u64>,
    pub missing_in_target: Vec<RecordKey>,
    pub unexpected_in_target: Vec<RecordKey>,
    pub digest_mismatches: Vec<RecordKey>,
    /// Live target records whose relationship target is absent or tombstoned.
    pub dangling_references: Vec<(RecordKey, RecordKey)>,
}

impl ComparisonReport {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.source_counts == self.target_counts
            && self.missing_in_target.is_empty()
            && self.unexpected_in_target.is_empty()
            && self.digest_mismatches.is_empty()
            && self.dangling_references.is_empty()
    }

    #[must_use]
    pub fn divergence_count(&self) -> usize {
        self.missing_in_target.len()
            + self.unexpected_in_target.len()
            + self.digest_mismatches.len()
            + self.dangling_references.len()
    }
}

fn index_records(records: &[MigrationRecord]) -> BTreeMap<RecordKey, &MigrationRecord> {
    records
        .iter()
        .map(|record| ((record.kind.clone(), record.id), record))
        .collect()
}

/// Compare two record sets by canonical identity, digest, counts, and references.
#[must_use]
pub fn compare_records(source: &[MigrationRecord], target: &[MigrationRecord]) -> ComparisonReport {
    let counts = |records: &[MigrationRecord]| {
        let mut counts = BTreeMap::new();
        for record in records {
            *counts.entry(record.kind.clone()).or_insert(0_u64) += 1;
        }
        counts
    };
    let (source_index, target_index) = (index_records(source), index_records(target));
    let mut report = ComparisonReport {
        source_counts: counts(source),
        target_counts: counts(target),
        ..ComparisonReport::default()
    };
    for (key, record) in &source_index {
        match target_index.get(key) {
            None => report.missing_in_target.push(key.clone()),
            Some(target)
                if target.digest != record.digest || target.tombstone != record.tombstone =>
            {
                report.digest_mismatches.push(key.clone());
            }
            Some(_) => {}
        }
    }
    report.unexpected_in_target = target_index
        .keys()
        .filter(|key| !source_index.contains_key(*key))
        .cloned()
        .collect();
    for (key, record) in &target_index {
        if record.tombstone {
            continue;
        }
        for reference in &record.references {
            if target_index
                .get(reference)
                .is_none_or(|target| target.tombstone)
            {
                report
                    .dangling_references
                    .push((key.clone(), reference.clone()));
            }
        }
    }
    report
}

/// Which provider generation currently accepts authoritative writes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Authority {
    Source,
    Target,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MigrationPhase {
    Planned,
    Exported,
    Imported,
    Verified,
    CapturingDelta,
    DeltaApplied,
    CutOver,
    RolledBack,
    Retired,
    Aborted,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum MigrationError {
    #[error("step {step} is not allowed in phase {phase:?}")]
    InvalidTransition {
        step: &'static str,
        phase: MigrationPhase,
    },
    #[error("import stream digest does not match the export")]
    StreamMismatch,
    #[error("comparison found {0} divergence(s); the step is refused")]
    Divergent(usize),
    #[error("the rollback window has not elapsed")]
    WindowOpen,
    #[error("rollback is unsafe: the old provider missed post-cutover writes")]
    ReverseReplicationBroken,
    #[error("retirement requires explicit operator approval")]
    ApprovalRequired,
    #[error("invalid migration parameter: {0}")]
    Invalid(&'static str),
}

/// A provider migration and its binding generations.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StorageMigration {
    pub migration_id: String,
    pub phase: MigrationPhase,
    pub source_generation: u64,
    pub target_generation: u64,
    /// Binding generation that routes authoritative writes; strictly increasing.
    pub active_generation: u64,
    pub authority: Authority,
    pub export_digest: Option<[u8; 32]>,
    pub exported_records: u64,
    pub rollback_window_millis: i64,
    pub cutover_at_millis: Option<i64>,
    /// Shadow writes that failed; before cutover they forbid cutover, after cutover
    /// they make rollback unsafe (ADR 0006).
    pub shadow_failures: u64,
}

impl StorageMigration {
    pub fn plan(
        migration_id: &str,
        source_generation: u64,
        rollback_window_millis: i64,
        capabilities_compatible: bool,
    ) -> Result<Self, MigrationError> {
        if migration_id.is_empty() || rollback_window_millis <= 0 {
            return Err(MigrationError::Invalid(
                "migration ID and rollback window are required",
            ));
        }
        if !capabilities_compatible {
            return Err(MigrationError::Invalid(
                "target capabilities are not compatible",
            ));
        }
        let target_generation = source_generation
            .checked_add(1)
            .ok_or(MigrationError::Invalid("generation overflow"))?;
        Ok(Self {
            migration_id: migration_id.to_owned(),
            phase: MigrationPhase::Planned,
            source_generation,
            target_generation,
            active_generation: source_generation,
            authority: Authority::Source,
            export_digest: None,
            exported_records: 0,
            rollback_window_millis,
            cutover_at_millis: None,
            shadow_failures: 0,
        })
    }

    fn require(
        &self,
        step: &'static str,
        allowed: &[MigrationPhase],
    ) -> Result<(), MigrationError> {
        if allowed.contains(&self.phase) {
            Ok(())
        } else {
            Err(MigrationError::InvalidTransition {
                step,
                phase: self.phase,
            })
        }
    }

    pub fn record_export(
        &mut self,
        stream_digest: [u8; 32],
        records: u64,
    ) -> Result<(), MigrationError> {
        self.require("export", &[MigrationPhase::Planned])?;
        self.export_digest = Some(stream_digest);
        self.exported_records = records;
        self.phase = MigrationPhase::Exported;
        Ok(())
    }

    pub fn record_import(
        &mut self,
        stream_digest: [u8; 32],
        records: u64,
    ) -> Result<(), MigrationError> {
        self.require("import", &[MigrationPhase::Exported])?;
        if self.export_digest != Some(stream_digest) || self.exported_records != records {
            return Err(MigrationError::StreamMismatch);
        }
        self.phase = MigrationPhase::Imported;
        Ok(())
    }

    pub fn record_verification(&mut self, report: &ComparisonReport) -> Result<(), MigrationError> {
        self.require("verify", &[MigrationPhase::Imported])?;
        if !report.is_clean() {
            return Err(MigrationError::Divergent(report.divergence_count()));
        }
        self.phase = MigrationPhase::Verified;
        Ok(())
    }

    /// Step 6: writes go to the source (authority) and are shadowed to the target.
    pub fn start_delta_capture(&mut self) -> Result<(), MigrationError> {
        self.require("start delta capture", &[MigrationPhase::Verified])?;
        self.shadow_failures = 0;
        self.phase = MigrationPhase::CapturingDelta;
        Ok(())
    }

    pub fn record_shadow_failure(&mut self) {
        self.shadow_failures = self.shadow_failures.saturating_add(1);
    }

    /// Step 7: the final delta comparison must be clean and no shadow write may have failed.
    pub fn record_delta(&mut self, report: &ComparisonReport) -> Result<(), MigrationError> {
        self.require("apply delta", &[MigrationPhase::CapturingDelta])?;
        if !report.is_clean() || self.shadow_failures > 0 {
            return Err(MigrationError::Divergent(
                report.divergence_count()
                    + usize::try_from(self.shadow_failures).unwrap_or(usize::MAX),
            ));
        }
        self.phase = MigrationPhase::DeltaApplied;
        Ok(())
    }

    /// Step 8: atomically switch the binding generation; the source keeps receiving
    /// shadow writes for the rollback window.
    pub fn cutover(&mut self, now_millis: i64) -> Result<(), MigrationError> {
        self.require("cutover", &[MigrationPhase::DeltaApplied])?;
        self.active_generation = self.target_generation;
        self.authority = Authority::Target;
        self.cutover_at_millis = Some(now_millis);
        self.shadow_failures = 0;
        self.phase = MigrationPhase::CutOver;
        Ok(())
    }

    /// Return authority to the source. After cutover this is only safe while every
    /// post-cutover write also reached the source.
    pub fn rollback(&mut self) -> Result<(), MigrationError> {
        self.require(
            "rollback",
            &[
                MigrationPhase::CapturingDelta,
                MigrationPhase::DeltaApplied,
                MigrationPhase::CutOver,
            ],
        )?;
        if self.phase == MigrationPhase::CutOver && self.shadow_failures > 0 {
            return Err(MigrationError::ReverseReplicationBroken);
        }
        // A new generation fences any writer still routed at the target generation.
        self.active_generation = self
            .active_generation
            .checked_add(1)
            .ok_or(MigrationError::Invalid("generation overflow"))?;
        self.authority = Authority::Source;
        self.phase = MigrationPhase::RolledBack;
        Ok(())
    }

    /// Step 10: retire the source only after the window and explicit approval.
    pub fn retire(
        &mut self,
        now_millis: i64,
        operator_approved: bool,
    ) -> Result<(), MigrationError> {
        self.require("retire", &[MigrationPhase::CutOver])?;
        let cutover = self
            .cutover_at_millis
            .ok_or(MigrationError::Invalid("missing cutover time"))?;
        if now_millis < cutover.saturating_add(self.rollback_window_millis) {
            return Err(MigrationError::WindowOpen);
        }
        if !operator_approved {
            return Err(MigrationError::ApprovalRequired);
        }
        self.phase = MigrationPhase::Retired;
        Ok(())
    }

    pub fn abort(&mut self) -> Result<(), MigrationError> {
        self.require(
            "abort",
            &[
                MigrationPhase::Planned,
                MigrationPhase::Exported,
                MigrationPhase::Imported,
                MigrationPhase::Verified,
            ],
        )?;
        self.phase = MigrationPhase::Aborted;
        Ok(())
    }

    /// The provider that must receive shadow writes, if any.
    #[must_use]
    pub fn shadow(&self) -> Option<Authority> {
        match self.phase {
            MigrationPhase::CapturingDelta | MigrationPhase::DeltaApplied => {
                Some(Authority::Target)
            }
            MigrationPhase::CutOver => Some(Authority::Source),
            _ => None,
        }
    }
}

/// One canonical write routed by the dual-write path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CanonicalWrite {
    pub kind: String,
    pub id: [u8; 16],
    /// Canonical capsule bytes (ADR 0005); providers map them natively.
    pub canonical: Vec<u8>,
    pub expected_revision: Option<u64>,
    /// Binding generation the write was routed under; writers fence older ones.
    pub generation: u64,
}

/// Deduplicated reference keys helper for adapters.
#[must_use]
pub fn unique_references(references: impl IntoIterator<Item = RecordKey>) -> Vec<RecordKey> {
    references
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: &str, id: u8, digest: u8, references: Vec<RecordKey>) -> MigrationRecord {
        MigrationRecord {
            kind: kind.to_owned(),
            id: [id; 16],
            digest: [digest; 32],
            tombstone: false,
            references,
        }
    }

    #[test]
    fn comparison_detects_every_divergence_class() {
        let user = record("core.user", 1, 1, Vec::new());
        let creature = record(
            "core.creature",
            2,
            2,
            vec![("core.user".to_owned(), [1; 16])],
        );
        assert!(
            compare_records(
                &[user.clone(), creature.clone()],
                &[creature.clone(), user.clone()]
            )
            .is_clean()
        );

        let changed = record("core.user", 1, 9, Vec::new());
        let report = compare_records(
            &[user.clone(), creature.clone()],
            &[changed, creature.clone()],
        );
        assert_eq!(report.digest_mismatches.len(), 1);

        let report = compare_records(
            &[user.clone(), creature.clone()],
            std::slice::from_ref(&creature),
        );
        assert_eq!(report.missing_in_target.len(), 1);
        assert_eq!(report.dangling_references.len(), 1);
        assert!(!report.is_clean());

        let extra = record("core.program", 3, 3, Vec::new());
        let report = compare_records(std::slice::from_ref(&user), &[user.clone(), extra]);
        assert_eq!(report.unexpected_in_target.len(), 1);
    }

    fn verified() -> StorageMigration {
        let mut migration = StorageMigration::plan("m1", 4, 1_000, true).unwrap();
        migration.record_export([7; 32], 2).unwrap();
        migration.record_import([7; 32], 2).unwrap();
        migration
            .record_verification(&ComparisonReport::default())
            .unwrap();
        migration
    }

    #[test]
    fn happy_path_switches_generation_and_keeps_rollback_shadow() {
        let mut migration = verified();
        migration.start_delta_capture().unwrap();
        assert_eq!(migration.shadow(), Some(Authority::Target));
        migration
            .record_delta(&ComparisonReport::default())
            .unwrap();
        migration.cutover(100).unwrap();
        assert_eq!(migration.authority, Authority::Target);
        assert_eq!(migration.active_generation, 5);
        assert_eq!(migration.shadow(), Some(Authority::Source));
        assert_eq!(migration.retire(500, true), Err(MigrationError::WindowOpen));
        assert_eq!(
            migration.retire(1_100, false),
            Err(MigrationError::ApprovalRequired)
        );
        migration.retire(1_100, true).unwrap();
        assert_eq!(migration.phase, MigrationPhase::Retired);
    }

    #[test]
    fn guards_refuse_unsafe_steps() {
        let mut migration = StorageMigration::plan("m1", 4, 1_000, true).unwrap();
        assert!(matches!(
            migration.cutover(1),
            Err(MigrationError::InvalidTransition { .. })
        ));
        migration.record_export([7; 32], 2).unwrap();
        assert_eq!(
            migration.record_import([8; 32], 2),
            Err(MigrationError::StreamMismatch)
        );
        migration.record_import([7; 32], 2).unwrap();
        let divergent = ComparisonReport {
            missing_in_target: vec![("core.user".to_owned(), [1; 16])],
            ..ComparisonReport::default()
        };
        assert_eq!(
            migration.record_verification(&divergent),
            Err(MigrationError::Divergent(1))
        );
        assert!(StorageMigration::plan("m", 1, 1, false).is_err());

        let mut migration = verified();
        migration.start_delta_capture().unwrap();
        migration.record_shadow_failure();
        assert!(matches!(
            migration.record_delta(&ComparisonReport::default()),
            Err(MigrationError::Divergent(1))
        ));
    }

    #[test]
    fn rollback_is_fenced_and_forbidden_after_lost_reverse_writes() {
        let mut migration = verified();
        migration.start_delta_capture().unwrap();
        migration
            .record_delta(&ComparisonReport::default())
            .unwrap();
        migration.cutover(100).unwrap();
        let mut safe = migration.clone();
        safe.rollback().unwrap();
        assert_eq!(safe.authority, Authority::Source);
        assert_eq!(safe.active_generation, 6);
        migration.record_shadow_failure();
        assert_eq!(
            migration.rollback(),
            Err(MigrationError::ReverseReplicationBroken)
        );
    }
}
