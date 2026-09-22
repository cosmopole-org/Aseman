//! Behavioral boundaries required by application use cases.
#![forbid(unsafe_code)]

use aseman_domain::Uuid;
use aseman_domain::authority::{AuditRecord, AuditedDecision, PolicyDecision, PolicyRequest};
use aseman_domain::blob::BlobEvidence;
use aseman_domain::capability::Grant;
use aseman_domain::creature::{CreatureRecord, MetadataKind};
use aseman_domain::gateway::GatewayRoute;
use aseman_domain::guest::{GuestKvOperation, GuestKvOutcome};
use aseman_domain::identity::{
    AuthenticationError, Challenge, IdentityKey, Introduction, KeyDescription, KeyPurpose, Proof,
    Subject,
};
use aseman_domain::program::{
    ArtifactRole, EntityArtifact, EntityRecord, ProgramAlarm, ProgramRecord, ResourceEntityRef,
    VmResourceEntity, VmResourceStore,
};
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
pub mod vmm;

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

/// The trusted catalog of creature guest databases (`core.guest_database_binding`,
/// A306). Routing reads only this catalog; no caller ever names a database or role.
pub trait CreatureDatabaseBindings: Send + Sync {
    /// The creature's current binding, active or not.
    fn binding_for(&self, creature: CreatureId) -> PortResult<Option<CreatureDatabaseBinding>>;
    /// Record the creature's binding, replacing the current one. `Conflict` when its
    /// generation is lower than the recorded one (rollback raises the generation).
    fn record_binding(&self, binding: &CreatureDatabaseBinding) -> PortResult<()>;
}

/// Legacy key/value operations on one creature's guest database (A405, ADR 0021). The
/// binding is the one the server resolved from the authenticated workload.
pub trait GuestKv: Send + Sync {
    fn execute(
        &self,
        binding: &CreatureDatabaseBinding,
        operation: &GuestKvOperation,
    ) -> PortResult<GuestKvOutcome>;
}

/// A policy provider (ADR 0008, A404). Providers decide from the request alone and
/// perform no I/O; callers treat an error as a denial.
pub trait PolicyDecisionPort: Send + Sync {
    fn decide(&self, request: &PolicyRequest) -> PortResult<PolicyDecision>;
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

/// Program records and the machine-to-program relation, keyed by legacy identity.
pub trait ProgramDirectory: Send + Sync {
    fn program(&self, program_id: &str) -> PortResult<Option<ProgramRecord>>;
    /// Programs in identity order, paged with [`aseman_domain::creature::legacy_page`].
    fn programs(&self, offset: i64, count: Option<i64>) -> PortResult<Vec<ProgramRecord>>;
    /// The programs a machine owns, in identity order.
    fn programs_of_machine(&self, machine_id: &str) -> PortResult<Vec<ProgramRecord>>;
    /// Register a new program under its machine. `Conflict` when the identity is taken.
    fn create_program(&self, record: &ProgramRecord) -> PortResult<()>;
    /// Replace a program's fields; a changed machine moves the relation. `NotFound`
    /// when absent.
    fn update_program(&self, record: &ProgramRecord) -> PortResult<()>;
    /// Remove a program and its relation. Removing an absent program succeeds.
    fn delete_program(&self, program_id: &str) -> PortResult<()>;
}

/// Program metadata documents (ADR 0016, `ProgMeta` / `core.program_metadata`),
/// crossing the port as JSON object text.
pub trait ProgramMetadata: Send + Sync {
    /// The object at a dotted legacy `path` under `metadata`, as JSON object text.
    fn program_metadata(&self, program_id: &str, path: &str) -> PortResult<Option<String>>;
    /// Deep-merge a JSON object into the document, creating it when absent, as legacy
    /// `put_json(.., merge = true)` does. Any other JSON is `Failed`.
    fn merge_program_metadata(&self, program_id: &str, document: &str) -> PortResult<()>;
    /// Remove the document. Removing an absent document succeeds.
    fn delete_program_metadata(&self, program_id: &str) -> PortResult<()>;
}

/// A program's pending alarm (legacy `vmAlarm*`, target `core.program_alarm`).
pub trait ProgramAlarms: Send + Sync {
    fn alarm(&self, program_id: &str) -> PortResult<Option<ProgramAlarm>>;
    /// Replace the program's alarm.
    fn set_alarm(&self, program_id: &str, alarm: &ProgramAlarm) -> PortResult<()>;
    /// Remove the program's alarm. Removing an absent alarm succeeds.
    fn clear_alarm(&self, program_id: &str) -> PortResult<()>;
}

/// Store metadata documents (ADR 0016, `StoreMeta` / `core.store_metadata`),
/// crossing the port as JSON object text.
pub trait StoreMetadata: Send + Sync {
    /// The object at a dotted legacy `path` under `metadata`, as JSON object text.
    fn store_metadata(&self, store_id: &str, path: &str) -> PortResult<Option<String>>;
    /// Deep-merge a JSON object into the document, creating it when absent, as legacy
    /// `put_json(.., merge = true)` does. Any other JSON is `Failed`.
    fn merge_store_metadata(&self, store_id: &str, document: &str) -> PortResult<()>;
    /// Remove the document. Removing an absent document succeeds.
    fn delete_store_metadata(&self, store_id: &str) -> PortResult<()>;
}

/// Creature HTTP routes to program entities, their reverse index, and the
/// username-local-part aliases that address them.
pub trait GatewayRoutes: Send + Sync {
    fn route(&self, creature_id: &str, path: &str) -> PortResult<Option<GatewayRoute>>;
    /// The route an entity is exposed on, as `(creature_id, path)`.
    fn route_of_entity(
        &self,
        program_id: &str,
        entity_id: &str,
    ) -> PortResult<Option<(String, String)>>;
    /// Store a route and point its entity's reverse index at it.
    fn put_route(&self, route: &GatewayRoute) -> PortResult<()>;
    /// Remove a route, and its entity's reverse index when that still names it.
    fn delete_route(&self, creature_id: &str, path: &str) -> PortResult<()>;
    /// The creature a username local part addresses.
    fn alias(&self, local_part: &str) -> PortResult<Option<String>>;
    /// Record that a username local part addresses `creature_id`.
    fn put_alias(&self, local_part: &str, creature_id: &str) -> PortResult<()>;
}

/// VM resource stores owned by machines.
pub trait VmResourceStores: Send + Sync {
    fn resource_store(&self, store_id: &str) -> PortResult<Option<VmResourceStore>>;
    /// Resource store ids in id order, of one machine or of all machines.
    fn resource_stores(&self, machine_id: Option<&str>) -> PortResult<Vec<String>>;
    /// Create or update a store: `name` is replaced, an empty `machine_id` keeps the
    /// current owner (LD-21), and `metadata` (a JSON object) is deep-merged.
    /// Creating a store without a machine is `Failed`.
    fn put_resource_store(
        &self,
        store_id: &str,
        name: &str,
        machine_id: &str,
        metadata: &str,
    ) -> PortResult<()>;
    /// Remove a store and its ownership. Removing an absent store succeeds.
    fn delete_resource_store(&self, store_id: &str) -> PortResult<()>;
}

/// Program entities, their deployed files, and their configuration (legacy `Entity`,
/// the `vmEntity*` links and `Json::ProxyEntity`; target `core.entity`,
/// `core.entity_artifact` and `core.entity_config`). Entities are never deleted, as in
/// legacy.
pub trait EntityDirectory: Send + Sync {
    fn entity(&self, program_id: &str, entity_id: &str) -> PortResult<Option<EntityRecord>>;
    /// Create or replace an entity. `NotFound` when its program does not exist.
    fn put_entity(&self, entity: &EntityRecord) -> PortResult<()>;
    fn artifact(
        &self,
        program_id: &str,
        entity_id: &str,
        role: ArtifactRole,
    ) -> PortResult<Option<EntityArtifact>>;
    /// Record the stored file of a role, replacing the previous one. `NotFound` when
    /// the entity does not exist.
    fn put_artifact(
        &self,
        program_id: &str,
        entity_id: &str,
        role: ArtifactRole,
        evidence: &BlobEvidence,
    ) -> PortResult<()>;
    /// The programs with an entity that has a primary file, in identity byte order.
    fn deployed_programs(&self) -> PortResult<Vec<String>>;
    /// The entity's configuration document as JSON object text.
    fn entity_config(&self, program_id: &str, entity_id: &str) -> PortResult<Option<String>>;
    /// Deep-merge a JSON object into the configuration, as legacy
    /// `put_json(.., merge = true)`. `NotFound` when the entity does not exist; any
    /// other JSON is `Failed`.
    fn merge_entity_config(
        &self,
        program_id: &str,
        entity_id: &str,
        document: &str,
    ) -> PortResult<()>;
}

/// VM resource entities (legacy `Json::VmResourceEntity` with its data file, target
/// `core.vm_resource_entity`). An invalid reference
/// ([`aseman_domain::program::ResourceEntityRef::is_valid`]) is `Failed`.
pub trait VmResourceEntities: Send + Sync {
    fn resource_entity(&self, entity: &ResourceEntityRef) -> PortResult<Option<VmResourceEntity>>;
    /// Create or update an entity: deep-merge the payload JSON object, as legacy
    /// `put_json(.., merge = true)`, and record its stored data. `NotFound` when the
    /// resource store does not exist.
    fn put_resource_entity(
        &self,
        entity: &ResourceEntityRef,
        payload: &str,
        data: &BlobEvidence,
    ) -> PortResult<()>;
    /// Remove an entity record. Removing an absent entity succeeds.
    fn delete_resource_entity(&self, entity: &ResourceEntityRef) -> PortResult<()>;
}

/// Identity keys by key ID and by subject (A401 section 9, `core.identity_key`).
pub trait KeyDirectory: Send + Sync {
    fn key(&self, key_id: &str) -> PortResult<Option<IdentityKey>>;
    /// Every epoch of one subject and purpose, in epoch order.
    fn epochs(&self, subject: &Subject, purpose: KeyPurpose) -> PortResult<Vec<IdentityKey>>;
    /// Record a new key epoch. `Conflict` when its key ID, or its epoch for the subject
    /// and purpose, is already recorded.
    fn register(&self, key: &IdentityKey) -> PortResult<()>;
    /// Record when the key stopped being current. An earlier recorded time is kept.
    /// `NotFound` when absent.
    fn retire(&self, key_id: &str, at_millis: i64) -> PortResult<()>;
    /// Record the key's revocation. An earlier recorded time is kept. `NotFound` when
    /// absent.
    fn revoke(&self, key_id: &str, at_millis: i64) -> PortResult<()>;
}

/// Used nonces (A401 "Replay"), shared by every verifier of one audience.
pub trait ReplayGuard: Send + Sync {
    /// Atomically record `(key_id, nonce)` until `retain_until_millis`. `Ok(true)` on
    /// first use, `Ok(false)` when the pair is already recorded and not yet expired at
    /// `now_millis`.
    fn record_nonce(
        &self,
        key_id: &str,
        nonce: &[u8],
        retain_until_millis: i64,
        now_millis: i64,
    ) -> PortResult<bool>;
}

/// The cryptography of A401 proofs.
pub trait IdentityVerifier: Send + Sync {
    /// Verify `proof`'s signature with `key` (A401 validation step 11).
    ///
    /// # Errors
    ///
    /// `BadSignature`, `UnsupportedAlgorithm`, `UnknownKey` when `key` is not the key
    /// the proof names, or `Malformed` for an undecodable stored key.
    fn verify(&self, proof: &Proof, key: &IdentityKey) -> Result<(), AuthenticationError>;
    /// SHA-256 of a request body (A401 validation step 10).
    fn body_digest(&self, body: &[u8]) -> [u8; 32];
    /// Decode an A401 key encoding and name it.
    ///
    /// # Errors
    ///
    /// `UnsupportedVersion`, `UnsupportedAlgorithm`, or `Malformed`.
    fn describe_key(&self, public_key: &[u8]) -> Result<KeyDescription, AuthenticationError>;
    /// The canonical bytes of an introduction (A401 section 8), which its proof signs as
    /// the body.
    fn introduction_bytes(&self, introduction: &Introduction) -> Vec<u8>;
}

/// One-time server challenges (A401 "Replay").
pub trait ChallengeStore: Send + Sync {
    /// Issue a fresh random nonce (32 bytes) for `subject` and `audience`.
    fn issue(
        &self,
        subject: &Subject,
        audience: &str,
        expires_at_millis: i64,
    ) -> PortResult<Challenge>;
    /// Atomically consume the challenge with `nonce` if it was issued to `subject` for
    /// `audience` and has not expired at `now_millis`. `Ok(false)` otherwise; a nonce is
    /// consumed at most once.
    fn consume(
        &self,
        nonce: &[u8],
        subject: &Subject,
        audience: &str,
        now_millis: i64,
    ) -> PortResult<bool>;
}

/// Durable capability grants (A403, `core.capability_grant`).
pub trait GrantStore: Send + Sync {
    fn grant(&self, id: Uuid) -> PortResult<Option<Grant>>;
    /// Every grant held by `subject`, live or not; evaluation checks liveness.
    fn grants_of(&self, subject: &Subject) -> PortResult<Vec<Grant>>;
    /// The grants delegated directly from `parent`.
    fn children(&self, parent: Uuid) -> PortResult<Vec<Grant>>;
    /// Record a new grant. `Conflict` when its ID is taken.
    fn put(&self, grant: &Grant) -> PortResult<()>;
    /// Record the grant's revocation; an earlier recorded time is kept. `NotFound` when
    /// absent.
    fn revoke(&self, id: Uuid, at_millis: i64) -> PortResult<()>;
}

/// The append-only record of policy decisions (`audit.event`), one chained stream
/// per actor.
pub trait DecisionAudit: Send + Sync {
    /// Append a decision to its actor's stream; returns its sequence.
    fn record(&self, record: &AuditRecord) -> PortResult<u64>;
    /// The actor's stream in sequence order.
    fn stream(&self, actor: &str) -> PortResult<Vec<AuditedDecision>>;
}

/// File bytes under provider-neutral keys (ADR 0027). Records keep only the
/// [`BlobEvidence`] a put returns.
pub trait BlobStore: Send + Sync {
    /// Store `bytes` under `key`. Without `overwrite`, an existing blob is a
    /// `Conflict`. An invalid key ([`aseman_domain::blob::valid_blob_key`]) is
    /// `Failed`.
    fn put_blob(
        &self,
        key: &str,
        bytes: &[u8],
        media_type: &str,
        overwrite: bool,
    ) -> PortResult<BlobEvidence>;
    fn blob(&self, key: &str) -> PortResult<Option<Vec<u8>>>;
    fn has_blob(&self, key: &str) -> PortResult<bool>;
    /// Remove a blob. Removing an absent blob succeeds.
    fn delete_blob(&self, key: &str) -> PortResult<()>;
    /// A local filesystem path to the blob, for runtimes that open files by path.
    fn local_path(&self, key: &str) -> PortResult<std::path::PathBuf>;
}

/// Store records as the store use cases see them.
pub trait StoreDirectory: Send + Sync {
    fn store(&self, store_id: &str) -> PortResult<Option<StoreRecord>>;
    /// Count one recorded signal against the store.
    fn record_signal(&self, store_id: &str) -> PortResult<()>;
    /// Stores in identity order, paged with [`aseman_domain::creature::legacy_page`].
    fn stores(&self, offset: i64, count: Option<i64>) -> PortResult<Vec<StoreRecord>>;
    /// Register a new store created by `creator_id`. `Conflict` when the identity is
    /// taken. Membership is granted separately through [`StoreAccess`].
    fn create_store(&self, record: &StoreRecord, creator_id: &str) -> PortResult<()>;
    /// Replace a store's fields. `NotFound` when absent.
    fn update_store(&self, record: &StoreRecord) -> PortResult<()>;
    /// Remove a store. Removing an absent store succeeds; memberships are left to
    /// [`StoreAccess`].
    fn delete_store(&self, store_id: &str) -> PortResult<()>;
    /// Drop a deleted creator's claim on a store that outlives it. Legacy removes
    /// the `creatorof` link; the capsule provider keeps the store's creator
    /// relationship to the tombstoned creature.
    fn release_creator(&self, store_id: &str, creator_id: &str) -> PortResult<()>;
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
