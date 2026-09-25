//! PostgreSQL as a provider behind the provider-neutral `CapsuleStore` seam.

use crate::{PostgresCapsuleRepository, PostgresStorageError};
use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery};

impl aseman_capsule::CapsuleStore for PostgresCapsuleRepository {
    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> aseman_capsule::CapsuleStoreResult<Option<CapsuleEnvelope>> {
        PostgresCapsuleRepository::get(self, kind, id).map_err(capsule_store_error)
    }

    fn put(
        &self,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> aseman_capsule::CapsuleStoreResult<()> {
        PostgresCapsuleRepository::put(self, capsule, expected_revision)
            .map_err(capsule_store_error)
    }

    fn put_all(
        &self,
        writes: &[(CapsuleEnvelope, Option<u64>)],
    ) -> aseman_capsule::CapsuleStoreResult<()> {
        PostgresCapsuleRepository::put_all(self, writes).map_err(capsule_store_error)
    }

    fn query(
        &self,
        query: &CapsuleQuery,
    ) -> aseman_capsule::CapsuleStoreResult<Vec<CapsuleEnvelope>> {
        PostgresCapsuleRepository::query(self, query).map_err(capsule_store_error)
    }
}

fn capsule_store_error(error: PostgresStorageError) -> aseman_capsule::CapsuleStoreError {
    match error {
        PostgresStorageError::Conflict => aseman_capsule::CapsuleStoreError::Conflict,
        other => aseman_capsule::CapsuleStoreError::Failed(other.to_string()),
    }
}

/// PostgreSQL behind the capsule seam with every write fenced at one binding
/// generation (A309): once the fence is raised past it, the node's writes are refused
/// with `Conflict` instead of landing in a provider it no longer owns.
pub struct FencedCapsuleStore {
    pub repository: PostgresCapsuleRepository,
    pub generation: u64,
}

impl aseman_capsule::CapsuleStore for FencedCapsuleStore {
    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> aseman_capsule::CapsuleStoreResult<Option<CapsuleEnvelope>> {
        self.repository.get(kind, id).map_err(capsule_store_error)
    }

    fn put(
        &self,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> aseman_capsule::CapsuleStoreResult<()> {
        self.repository
            .put_fenced(capsule, expected_revision, Some(self.generation))
            .map_err(capsule_store_error)
    }

    fn put_all(
        &self,
        writes: &[(CapsuleEnvelope, Option<u64>)],
    ) -> aseman_capsule::CapsuleStoreResult<()> {
        self.repository
            .put_all_fenced(writes, Some(self.generation))
            .map_err(capsule_store_error)
    }

    fn query(
        &self,
        query: &CapsuleQuery,
    ) -> aseman_capsule::CapsuleStoreResult<Vec<CapsuleEnvelope>> {
        self.repository.query(query).map_err(capsule_store_error)
    }
}
