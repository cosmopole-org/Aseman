//! Behavioral boundaries required by application use cases.
#![forbid(unsafe_code)]

use aseman_domain::creature::{CreatureRecord, MetadataKind};
use aseman_domain::signal_tags::LogQuery;
use aseman_domain::storage_migration::{
    CanonicalWrite, MigrationPhase, MigrationRecord, StorageMigration,
};
use aseman_domain::store::{StoreRecord, StoreSignal};
use aseman_domain::store_permissions::StorePermissions;
use aseman_domain::{CreatureDatabaseBinding, CreatureId, DesiredWorkload, Generation, WorkloadId};
use thiserror::Error;

#[cfg(feature = "conformance")]
pub mod conformance;

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
    /// An adapter failure whose message is part of the client-visible contract.
    #[error("{0}")]
    Failed(String),
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

/// Creature identity records, keyed by legacy identity. Balances are separate
/// ([`CreatureBalances`]): the target model keeps them in finance wallets.
pub trait CreatureDirectory: Send + Sync {
    fn creature(&self, creature_id: &str) -> PortResult<Option<CreatureRecord>>;
    fn creature_id_by_username(&self, username: &str) -> PortResult<Option<String>>;
    /// The first creature, in username order, whose username contains `fragment`.
    fn find_by_username_fragment(&self, fragment: &str) -> PortResult<Option<CreatureRecord>>;
    /// Creatures in identity order, optionally of one type, paged with
    /// [`aseman_domain::creature::legacy_page`].
    fn creatures(
        &self,
        creature_type: Option<&str>,
        offset: i64,
        count: Option<i64>,
    ) -> PortResult<Vec<CreatureRecord>>;
    /// Register a new creature. `Conflict` when the identity or the username is
    /// taken. Its balance is opened separately through [`CreatureBalances::open`],
    /// because balances stay with the finance subsystem (ADR 0017).
    fn create(&self, record: &CreatureRecord) -> PortResult<()>;
    /// Replace the identity fields; a changed username moves with the record.
    /// `NotFound` when absent, `Conflict` when the new username is taken.
    fn update(&self, record: &CreatureRecord) -> PortResult<()>;
    /// Remove the identity record. Absent records are already deleted.
    fn delete(&self, creature_id: &str) -> PortResult<()>;
}

/// Creature metadata documents (ADR 0016). Documents cross the port as JSON object
/// text, so the domain stays free of any JSON library.
pub trait CreatureMetadata: Send + Sync {
    /// The object at a dotted legacy `path` under [`aseman_domain::creature::METADATA_ROOT`],
    /// as JSON object text. `None` when the document, the path, or an object at the
    /// path is absent.
    fn metadata(
        &self,
        kind: MetadataKind,
        creature_id: &str,
        path: &str,
    ) -> PortResult<Option<String>>;
    /// Replace the whole document with a JSON object. Any other JSON is `Failed`.
    fn replace_metadata(
        &self,
        kind: MetadataKind,
        creature_id: &str,
        document: &str,
    ) -> PortResult<()>;
    /// Remove the document. Removing an absent document succeeds.
    fn delete_metadata(&self, kind: MetadataKind, creature_id: &str) -> PortResult<()>;
}

/// The creature type registry (ADR 0016 amendment). Specs cross the port as JSON
/// object text; an empty spec reads as an unregistered type, as in legacy.
pub trait CreatureTypes: Send + Sync {
    fn creature_type(&self, name: &str) -> PortResult<Option<String>>;
    /// Every registered type with a non-empty spec, in name order.
    fn creature_types(&self) -> PortResult<Vec<(String, String)>>;
    /// Register or replace a type's spec, a JSON object. Any other JSON is `Failed`.
    fn put_creature_type(&self, name: &str, spec: &str) -> PortResult<()>;
}

/// Creature balances in minor units. They change together with the finance
/// counters and journal, so they are served by the finance subsystem's provider,
/// which stays the legacy provider until P8 (ADR 0017).
pub trait CreatureBalances: Send + Sync {
    /// Open the balance of a newly registered creature. `Conflict` when it is open.
    fn open(&self, creature_id: &str, opening_balance: i64) -> PortResult<()>;
    /// Close the balance of a deleted creature. Closing an absent balance succeeds.
    fn close(&self, creature_id: &str) -> PortResult<()>;
    /// `NotFound` when the creature has no balance.
    fn balance(&self, creature_id: &str) -> PortResult<i64>;
    fn set_balance(&self, creature_id: &str, balance: i64) -> PortResult<()>;
}

/// Store records as the store use cases see them.
pub trait StoreDirectory: Send + Sync {
    fn store(&self, store_id: &str) -> PortResult<Option<StoreRecord>>;
    /// Count one recorded signal against the store.
    fn record_signal(&self, store_id: &str) -> PortResult<()>;
}

/// Per-member store permissions. An absent grant is the empty (deny-all) set.
pub trait StoreAccess: Send + Sync {
    fn permissions(&self, store_id: &str, member_id: &str) -> PortResult<StorePermissions>;
    fn set_permissions(
        &self,
        store_id: &str,
        member_id: &str,
        permissions: StorePermissions,
    ) -> PortResult<()>;
    /// Whether `member_id` belongs to the store (the guards' membership check).
    fn is_member(&self, store_id: &str, member_id: &str) -> PortResult<bool>;
    /// Every member of the store with its permissions, in member order.
    fn members(&self, store_id: &str) -> PortResult<Vec<(String, StorePermissions)>>;
    /// Every store the member belongs to, in store order.
    fn stores_of(&self, member_id: &str) -> PortResult<Vec<String>>;
    /// Make `member_id` a member with `permissions`.
    fn join(
        &self,
        store_id: &str,
        member_id: &str,
        permissions: StorePermissions,
    ) -> PortResult<()>;
    /// Remove `member_id` from the store entirely.
    fn leave(&self, store_id: &str, member_id: &str) -> PortResult<()>;
}

/// The durable store signal log. A failed append is an error, never a silent drop.
pub trait SignalLog: Send + Sync {
    fn append(
        &self,
        store_id: &str,
        sender_id: &str,
        data: &str,
        tags: &[String],
        time_millis: i64,
    ) -> PortResult<StoreSignal>;
    /// Newest first, filtered by an already validated query.
    fn history(&self, store_id: &str, query: &LogQuery) -> PortResult<Vec<StoreSignal>>;
}
