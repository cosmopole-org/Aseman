//! PostgreSQL as a provider behind the provider-neutral `CapsuleStore` seam.

use crate::{PostgresCapsuleRepository, PostgresStorageError};
use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery};

impl aseman_capsule_repositories::CapsuleStore for PostgresCapsuleRepository {
    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> aseman_capsule_repositories::CapsuleStoreResult<Option<CapsuleEnvelope>> {
        PostgresCapsuleRepository::get(self, kind, id).map_err(capsule_store_error)
    }

    fn put(
        &self,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> aseman_capsule_repositories::CapsuleStoreResult<()> {
        PostgresCapsuleRepository::put(self, capsule, expected_revision)
            .map_err(capsule_store_error)
    }

    fn put_all(
        &self,
        writes: &[(CapsuleEnvelope, Option<u64>)],
    ) -> aseman_capsule_repositories::CapsuleStoreResult<()> {
        PostgresCapsuleRepository::put_all(self, writes).map_err(capsule_store_error)
    }

    fn query(
        &self,
        query: &CapsuleQuery,
    ) -> aseman_capsule_repositories::CapsuleStoreResult<Vec<CapsuleEnvelope>> {
        PostgresCapsuleRepository::query(self, query).map_err(capsule_store_error)
    }
}

fn capsule_store_error(
    error: PostgresStorageError,
) -> aseman_capsule_repositories::CapsuleStoreError {
    match error {
        PostgresStorageError::Conflict => aseman_capsule_repositories::CapsuleStoreError::Conflict,
        other => aseman_capsule_repositories::CapsuleStoreError::Failed(other.to_string()),
    }
}
