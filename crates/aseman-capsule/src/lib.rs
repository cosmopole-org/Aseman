//! Typed domain repositories on the capsule protocol. Each repository
//! implements application ports over [`CapsuleStore`], so the same code runs against
//! PostgreSQL directly, the gRPC capsule provider, or any conforming provider.
#![forbid(unsafe_code)]

use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery};
use thiserror::Error;

pub mod audit;
pub mod auto;
pub mod capability;
pub mod coordination;
pub mod creature;
pub mod entity;
pub mod federation;
pub mod finance;
pub mod gateway;
pub mod guest_kv;
pub mod identity;
pub mod metering;
pub mod program;
pub mod realtime;
pub mod storage_adapter;
pub mod store;
mod support;
pub mod token_lock;
pub mod vmm;
pub mod workload;

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

/// A borrowed store is a store: adapters that own their store take a reference
/// for the length of one transaction, or an owned store for a service's lifetime.
impl<T: CapsuleStore + ?Sized> CapsuleStore for &T {
    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> CapsuleStoreResult<Option<CapsuleEnvelope>> {
        (**self).get(kind, id)
    }
    fn put(
        &self,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> CapsuleStoreResult<()> {
        (**self).put(capsule, expected_revision)
    }
    fn put_all(&self, writes: &[(CapsuleEnvelope, Option<u64>)]) -> CapsuleStoreResult<()> {
        (**self).put_all(writes)
    }
    fn query(&self, query: &CapsuleQuery) -> CapsuleStoreResult<Vec<CapsuleEnvelope>> {
        (**self).query(query)
    }
}
