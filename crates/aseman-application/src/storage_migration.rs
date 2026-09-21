//! A309 storage-migration use cases: verification, delta, cutover, rollback, retirement,
//! and the dual-write router that keeps the non-authoritative side in sync.

use crate::ApplicationError;
use aseman_domain::storage_migration::{
    Authority, CanonicalWrite, MigrationError, StorageMigration, compare_records,
};
use aseman_ports::{
    CanonicalRecordWriter, ClockPort, MigrationRecordSource, MigrationStateStore, PortError,
};

impl From<MigrationError> for ApplicationError {
    fn from(error: MigrationError) -> Self {
        ApplicationError::Denied(error.to_string())
    }
}

/// Drives the plan's provider-migration steps against durable, CAS-protected state.
pub struct StorageMigrationService<'a> {
    pub state: &'a dyn MigrationStateStore,
    pub source: &'a dyn MigrationRecordSource,
    pub target: &'a dyn MigrationRecordSource,
    pub clock: &'a dyn ClockPort,
}

impl StorageMigrationService<'_> {
    fn load(&self, migration_id: &str) -> Result<StorageMigration, ApplicationError> {
        self.state
            .load(migration_id)?
            .ok_or_else(|| ApplicationError::Denied(format!("unknown migration {migration_id}")))
    }

    fn step(
        &self,
        migration_id: &str,
        apply: impl FnOnce(&mut StorageMigration) -> Result<(), ApplicationError>,
    ) -> Result<StorageMigration, ApplicationError> {
        let mut migration = self.load(migration_id)?;
        let expected = migration.phase;
        apply(&mut migration)?;
        self.state.save(&migration, Some(expected))?;
        Ok(migration)
    }

    pub fn create(&self, migration: &StorageMigration) -> Result<(), ApplicationError> {
        Ok(self.state.save(migration, None)?)
    }

    pub fn record_export(
        &self,
        id: &str,
        digest: [u8; 32],
        records: u64,
    ) -> Result<StorageMigration, ApplicationError> {
        self.step(
            id,
            |migration| Ok(migration.record_export(digest, records)?),
        )
    }

    pub fn record_import(
        &self,
        id: &str,
        digest: [u8; 32],
        records: u64,
    ) -> Result<StorageMigration, ApplicationError> {
        self.step(
            id,
            |migration| Ok(migration.record_import(digest, records)?),
        )
    }

    /// Step 5: full semantic comparison of both providers.
    pub fn verify(&self, id: &str) -> Result<StorageMigration, ApplicationError> {
        let report = compare_records(&self.source.snapshot()?, &self.target.snapshot()?);
        self.step(id, |migration| Ok(migration.record_verification(&report)?))
    }

    pub fn start_delta_capture(&self, id: &str) -> Result<StorageMigration, ApplicationError> {
        self.step(id, |migration| Ok(migration.start_delta_capture()?))
    }

    /// Step 7: the final delta must compare clean with no failed shadow write.
    pub fn apply_delta(&self, id: &str) -> Result<StorageMigration, ApplicationError> {
        let report = compare_records(&self.source.snapshot()?, &self.target.snapshot()?);
        self.step(id, |migration| Ok(migration.record_delta(&report)?))
    }

    pub fn cutover(&self, id: &str) -> Result<StorageMigration, ApplicationError> {
        let now = self.clock.unix_millis();
        self.step(id, |migration| Ok(migration.cutover(now)?))
    }

    pub fn rollback(&self, id: &str) -> Result<StorageMigration, ApplicationError> {
        self.step(id, |migration| Ok(migration.rollback()?))
    }

    pub fn retire(
        &self,
        id: &str,
        operator_approved: bool,
    ) -> Result<StorageMigration, ApplicationError> {
        let now = self.clock.unix_millis();
        self.step(
            id,
            |migration| Ok(migration.retire(now, operator_approved)?),
        )
    }
}

/// Routes each write to the current authority, then shadows it to the other side.
pub struct DualWriteRouter<'a> {
    pub migration_id: &'a str,
    pub state: &'a dyn MigrationStateStore,
    pub source: &'a dyn CanonicalRecordWriter,
    pub target: &'a dyn CanonicalRecordWriter,
}

impl DualWriteRouter<'_> {
    fn writer(&self, side: Authority) -> &dyn CanonicalRecordWriter {
        match side {
            Authority::Source => self.source,
            Authority::Target => self.target,
        }
    }

    /// The authoritative write must succeed. A failed shadow write never fails the
    /// caller, but it is recorded durably: it blocks the delta before cutover and
    /// forbids rollback after it (ADR 0006).
    pub fn write(&self, write: &CanonicalWrite) -> Result<(), ApplicationError> {
        let migration = self
            .state
            .load(self.migration_id)?
            .ok_or(ApplicationError::Port(PortError::NotFound))?;
        let write = CanonicalWrite {
            generation: migration.active_generation,
            ..write.clone()
        };
        self.writer(migration.authority).write(&write)?;
        let Some(shadow) = migration.shadow() else {
            return Ok(());
        };
        if self.writer(shadow).write(&write).is_err() {
            loop {
                let mut current = self
                    .state
                    .load(self.migration_id)?
                    .ok_or(ApplicationError::Port(PortError::NotFound))?;
                let expected = current.phase;
                current.record_shadow_failure();
                match self.state.save(&current, Some(expected)) {
                    Err(PortError::Conflict) => continue,
                    other => break other?,
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_domain::storage_migration::{MigrationPhase, MigrationRecord};
    use aseman_ports::PortResult;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryState(Mutex<Option<StorageMigration>>);

    impl MigrationStateStore for MemoryState {
        fn load(&self, _: &str) -> PortResult<Option<StorageMigration>> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn save(
            &self,
            migration: &StorageMigration,
            expected: Option<MigrationPhase>,
        ) -> PortResult<()> {
            let mut stored = self.0.lock().unwrap();
            if stored.as_ref().map(|current| current.phase) != expected {
                return Err(PortError::Conflict);
            }
            *stored = Some(migration.clone());
            Ok(())
        }
    }

    /// A provider that is both a record source and a fenced writer.
    #[derive(Default)]
    struct MemoryProvider {
        records: Mutex<Vec<MigrationRecord>>,
        fenced_below: Mutex<u64>,
        failing: Mutex<bool>,
    }

    impl MigrationRecordSource for MemoryProvider {
        fn snapshot(&self) -> PortResult<Vec<MigrationRecord>> {
            Ok(self.records.lock().unwrap().clone())
        }
    }

    impl CanonicalRecordWriter for MemoryProvider {
        fn write(&self, write: &CanonicalWrite) -> PortResult<()> {
            if *self.failing.lock().unwrap() {
                return Err(PortError::Unavailable("provider down"));
            }
            if write.generation < *self.fenced_below.lock().unwrap() {
                return Err(PortError::Conflict);
            }
            let mut records = self.records.lock().unwrap();
            records.retain(|record| {
                (record.kind.as_str(), record.id) != (write.kind.as_str(), write.id)
            });
            records.push(MigrationRecord {
                kind: write.kind.clone(),
                id: write.id,
                digest: [write.canonical.len() as u8; 32],
                tombstone: false,
                references: Vec::new(),
            });
            Ok(())
        }
    }

    struct FixedClock(i64);
    impl ClockPort for FixedClock {
        fn unix_millis(&self) -> i64 {
            self.0
        }
    }

    fn write(id: u8, bytes: usize) -> CanonicalWrite {
        CanonicalWrite {
            kind: "core.user".to_owned(),
            id: [id; 16],
            canonical: vec![0; bytes],
            expected_revision: None,
            generation: 0,
        }
    }

    #[test]
    fn full_protocol_runs_dual_write_through_cutover_and_retirement() {
        let state = MemoryState::default();
        let (source, target) = (MemoryProvider::default(), MemoryProvider::default());
        source.write(&write(1, 3)).unwrap();
        target.write(&write(1, 3)).unwrap();
        let service = StorageMigrationService {
            state: &state,
            source: &source,
            target: &target,
            clock: &FixedClock(100),
        };
        service
            .create(&StorageMigration::plan("m1", 1, 50, true).unwrap())
            .unwrap();
        service.record_export("m1", [1; 32], 1).unwrap();
        service.record_import("m1", [1; 32], 1).unwrap();
        service.verify("m1").unwrap();
        service.start_delta_capture("m1").unwrap();

        let router = DualWriteRouter {
            migration_id: "m1",
            state: &state,
            source: &source,
            target: &target,
        };
        router.write(&write(2, 5)).unwrap();
        service.apply_delta("m1").unwrap();
        let cut = service.cutover("m1").unwrap();
        assert_eq!(cut.authority, Authority::Target);

        // After cutover the target is authoritative and the source still receives writes.
        router.write(&write(3, 7)).unwrap();
        assert_eq!(source.snapshot().unwrap().len(), 3);
        assert_eq!(target.snapshot().unwrap().len(), 3);
        assert!(service.retire("m1", true).is_err());
        let late = StorageMigrationService {
            clock: &FixedClock(200),
            ..service
        };
        assert_eq!(
            late.retire("m1", true).unwrap().phase,
            MigrationPhase::Retired
        );
    }

    #[test]
    fn divergence_and_lost_shadow_writes_block_cutover_and_rollback() {
        let state = MemoryState::default();
        let (source, target) = (MemoryProvider::default(), MemoryProvider::default());
        source.write(&write(1, 3)).unwrap();
        let service = StorageMigrationService {
            state: &state,
            source: &source,
            target: &target,
            clock: &FixedClock(100),
        };
        service
            .create(&StorageMigration::plan("m1", 1, 50, true).unwrap())
            .unwrap();
        service.record_export("m1", [1; 32], 1).unwrap();
        service.record_import("m1", [1; 32], 1).unwrap();
        // The target is missing the record: verification is refused.
        assert!(service.verify("m1").is_err());
        target.write(&write(1, 3)).unwrap();
        service.verify("m1").unwrap();
        service.start_delta_capture("m1").unwrap();

        let router = DualWriteRouter {
            migration_id: "m1",
            state: &state,
            source: &source,
            target: &target,
        };
        *target.failing.lock().unwrap() = true;
        // The authoritative write succeeds; the lost shadow write is recorded durably.
        router.write(&write(2, 5)).unwrap();
        assert_eq!(state.load("m1").unwrap().unwrap().shadow_failures, 1);
        assert!(service.apply_delta("m1").is_err());
        assert_eq!(service.rollback("m1").unwrap().authority, Authority::Source);
    }

    #[test]
    fn a_write_routed_under_an_old_generation_is_fenced() {
        let provider = MemoryProvider::default();
        *provider.fenced_below.lock().unwrap() = 5;
        assert_eq!(
            provider.write(&CanonicalWrite {
                generation: 4,
                ..write(1, 1)
            }),
            Err(PortError::Conflict)
        );
        provider
            .write(&CanonicalWrite {
                generation: 5,
                ..write(1, 1)
            })
            .unwrap();
    }
}
