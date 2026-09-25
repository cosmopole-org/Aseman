//! A309 adapters: canonical capsules as domain migration records, and the PostgreSQL
//! provider as a fenced record source/writer for dual write and comparison.

use crate::{PostgresCapsuleRepository, PostgresStorageError};
use aseman_contracts::capsule::CapsuleEnvelope;
use aseman_contracts::migration::semantic_digest;
use aseman_domain::storage_migration::{CanonicalWrite, MigrationRecord, unique_references};
use aseman_ports::{CanonicalRecordWriter, MigrationRecordSource, PortError, PortResult};

/// Provider-neutral comparison view of one canonical capsule.
pub fn migration_record(
    capsule: &CapsuleEnvelope,
) -> Result<MigrationRecord, PostgresStorageError> {
    Ok(MigrationRecord {
        kind: capsule.kind.0.clone(),
        id: capsule.id.0,
        // Semantic, not physical: a re-applied revision still compares equal (ADR 0005).
        digest: semantic_digest(capsule)
            .map_err(|error| PostgresStorageError::Invalid(error.to_string()))?,
        tombstone: capsule.tombstone,
        references: unique_references(
            capsule
                .relationships
                .iter()
                .map(|relationship| (relationship.target_kind.0.clone(), relationship.target_id.0)),
        ),
    })
}

fn port_error(error: PostgresStorageError) -> PortError {
    match error {
        PostgresStorageError::Conflict => PortError::Conflict,
        PostgresStorageError::Unsupported(_) => {
            PortError::Unsupported("capsule kind is not mapped")
        }
        PostgresStorageError::Invalid(_) => PortError::Denied("invalid capsule"),
        PostgresStorageError::Unavailable(_) => PortError::Unavailable("postgresql"),
    }
}

impl MigrationRecordSource for PostgresCapsuleRepository {
    fn snapshot(&self) -> PortResult<Vec<MigrationRecord>> {
        self.snapshot_all()
            .map_err(port_error)?
            .iter()
            .map(|capsule| migration_record(capsule).map_err(port_error))
            .collect()
    }
}

impl CanonicalRecordWriter for PostgresCapsuleRepository {
    fn write(&self, write: &CanonicalWrite) -> PortResult<()> {
        let capsule = CapsuleEnvelope::from_canonical_bytes(&write.canonical)
            .map_err(|_| PortError::Denied("write is not a canonical capsule"))?;
        if capsule.kind.0 != write.kind || capsule.id.0 != write.id {
            return Err(PortError::Denied(
                "write identity disagrees with its capsule",
            ));
        }
        self.put_fenced(&capsule, write.expected_revision, Some(write.generation))
            .map_err(port_error)
    }
}
