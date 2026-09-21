//! Typed domain repositories on the capsule protocol (Phase 3). Each repository
//! implements application ports over [`CapsuleStore`], so the same code runs against
//! PostgreSQL directly, the gRPC capsule provider, or any conforming provider.
#![forbid(unsafe_code)]

use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery};
use thiserror::Error;

pub mod creature;
pub mod store;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CapsuleStoreError {
    /// Optimistic revision or uniqueness conflict; the caller may retry.
    #[error("capsule revision conflict")]
    Conflict,
    #[error("{0}")]
    Failed(String),
}

pub type CapsuleStoreResult<T> = Result<T, CapsuleStoreError>;

/// The provider-neutral capsule seam repositories are written against.
pub trait CapsuleStore: Send + Sync {
    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> CapsuleStoreResult<Option<CapsuleEnvelope>>;
    fn put(
        &self,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> CapsuleStoreResult<()>;
    /// Apply every write in one transaction: all of them, or none.
    fn put_all(&self, writes: &[(CapsuleEnvelope, Option<u64>)]) -> CapsuleStoreResult<()>;
    fn query(&self, query: &CapsuleQuery) -> CapsuleStoreResult<Vec<CapsuleEnvelope>>;
}
