//! Behavioral boundaries required by application use cases.
#![forbid(unsafe_code)]

use aseman_domain::storage_migration::{
    CanonicalWrite, MigrationPhase, MigrationRecord, StorageMigration,
};
use aseman_domain::{CreatureDatabaseBinding, CreatureId, DesiredWorkload, Generation, WorkloadId};
use thiserror::Error;

pub type PortResult<T> = Result<T, PortError>;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PortError {
    #[error("not found")]
    NotFound,
    #[error("revision conflict")]
    Conflict,
    #[error("operation denied: {0}")]
    Denied(&'static str),
    #[error("dependency unavailable: {0}")]
    Unavailable(&'static str),
    #[error("deadline exceeded")]
    Deadline,
    #[error("unsupported capability: {0}")]
    Unsupported(&'static str),
}

pub trait WorkloadRepository: Send + Sync {
    fn get_desired(&self, id: WorkloadId) -> PortResult<Option<DesiredWorkload>>;
    fn put_desired(&self, workload: &DesiredWorkload, expected: Generation) -> PortResult<()>;
}

pub trait CreatureDatabaseBindings: Send + Sync {
    fn binding_for(&self, creature: CreatureId) -> PortResult<Option<CreatureDatabaseBinding>>;
}

pub trait PolicyDecisionPort: Send + Sync {
    fn authorize(&self, subject: &str, action: &str, resource: &str) -> PortResult<PolicyDecision>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyDecision {
    pub allowed: bool,
    pub policy_version: String,
    pub reason: String,
}

pub trait VmmPort: Send + Sync {
    fn apply_desired(&self, workload: &DesiredWorkload) -> PortResult<()>;
}

/// Time source supplied by composition so application behavior is deterministic in tests.
pub trait ClockPort: Send + Sync {
    fn unix_millis(&self) -> i64;
}

/// Public node identity material exposed by the unauthenticated bootstrap API.
pub trait ServerIdentityPort: Send + Sync {
    fn server_public_key(&self) -> PortResult<String>;
}

/// Current consensus peers exposed by the bootstrap API.
pub trait PeerDirectoryPort: Send + Sync {
    fn peer_servers(&self) -> PortResult<Vec<String>>;
}

/// Durable storage-migration state with compare-and-swap on the expected phase.
pub trait MigrationStateStore: Send + Sync {
    fn load(&self, migration_id: &str) -> PortResult<Option<StorageMigration>>;
    /// Persist `migration` only if the stored phase still equals `expected`
    /// (`None` means the migration must not exist yet); otherwise `Conflict`.
    fn save(
        &self,
        migration: &StorageMigration,
        expected: Option<MigrationPhase>,
    ) -> PortResult<()>;
}

/// A consistent snapshot of every canonical record held by one provider generation.
pub trait MigrationRecordSource: Send + Sync {
    fn snapshot(&self) -> PortResult<Vec<MigrationRecord>>;
}

/// Accepts one canonical write for one provider generation. Implementations must
/// reject (`Conflict`) a write whose `generation` is below the provider's fenced
/// minimum, so a writer routed before a cutover or rollback cannot land late.
pub trait CanonicalRecordWriter: Send + Sync {
    fn write(&self, write: &CanonicalWrite) -> PortResult<()>;
}
