//! Read-only legacy RocksDB snapshots and bounded canonical capsule export/import.
#![forbid(unsafe_code)]

use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleRelationship, CapsuleValue,
    OwnerScope, StorageClass, encode_value,
};
use aseman_contracts::migration::{
    CanonicalCapsuleExport, CapsuleExportCheckpoint, CapsuleExportHeader,
};
use rocksdb::{DB, IteratorMode, Options};
use rsa::RsaPublicKey;
use rsa::pkcs8::{DecodePublicKey, EncodePublicKey};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use thiserror::Error;

pub const DEFAULT_MAX_EXPORT_RECORDS: usize = 1_000_000;
pub const DEFAULT_MAX_EXPORT_BYTES: usize = 8 * 1024 * 1024 * 1024;
pub const DEFAULT_IMPORT_BATCH: usize = 1_000;
pub const DEFAULT_MAX_LEGACY_FILE_BYTES: usize = 1024 * 1024 * 1024;

mod bridge;
mod capsule;
mod cluster;
mod custody;
mod documents;
mod export;
mod finance;
mod graph;
mod guestkv;
mod hashgraph;
mod kv;
mod membership;
mod objects;
mod operational;
mod questdb;
mod secrets;
mod timeseries;
pub mod tuning;
mod vmintent;
mod vmresources;
mod vmstate;

pub use bridge::*;
pub use capsule::*;
pub use cluster::*;
pub use custody::*;
pub use documents::*;
pub use export::*;
pub use finance::*;
pub use graph::*;
pub use guestkv::*;
pub use hashgraph::*;
pub use kv::*;
pub use membership::*;
pub use objects::*;
pub use operational::*;
pub use questdb::*;
pub use secrets::*;
pub use timeseries::*;
pub use vmintent::*;
pub use vmresources::*;
pub use vmstate::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyPhysicalRecord {
    pub family: String,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacySnapshot {
    pub snapshot_id: String,
    pub records: Vec<LegacyPhysicalRecord>,
}

/// A two-pass view of legacy object columns and relationship/index records.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LegacySnapshotGraph {
    objects: BTreeMap<(String, String), BTreeMap<String, Vec<u8>>>,
    links: BTreeMap<String, Vec<u8>>,
    indexes: BTreeMap<String, Vec<u8>>,
    /// `json::{key}::{path}` records grouped by legacy key, then by path.
    documents: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    /// Reviewed raw operational keys (ADR 0020); any other raw key is `Unmapped`.
    raw: BTreeMap<String, Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyFinanceConfig {
    pub currency: String,
    pub scale: u8,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LegacyTransformEvidence {
    pub file_artifacts: BTreeMap<String, LegacyFileArtifactEvidence>,
    pub finance: Option<LegacyFinanceConfig>,
    /// Legacy ID origins owned by this installation (ADR 0018 member resolution).
    pub local_origins: BTreeSet<String>,
    /// Filesystem artifacts keyed by their exact legacy path (ADR 0022).
    pub path_artifacts: BTreeMap<String, LegacyPathArtifact>,
    /// The legacy `node-secret-key`, used only to authenticate ciphertext (ADR 0023).
    pub secret_master_key: Option<LegacySecretMasterKey>,
}

/// Runner-supplied evidence for a legacy on-disk artifact (ADR 0022).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegacyPathArtifact {
    /// The bytes were copied and verified.
    Present(LegacyFileArtifactEvidence),
    /// The runner checked the filesystem and the file does not exist; legacy
    /// deletes remove files while leaving their records behind.
    AttestedAbsent,
}

pub trait LegacyRecordSource {
    fn read_snapshot(
        &self,
        max_records: usize,
        max_bytes: usize,
    ) -> LegacyMigrationResult<LegacySnapshot>;
}

pub trait LegacyTransformer {
    fn manifest_digest(&self) -> [u8; 32];
    fn transform(
        &mut self,
        record: LegacyPhysicalRecord,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>>;
    fn finish(&mut self) -> LegacyMigrationResult<Vec<CapsuleEnvelope>>;
}

pub trait CapsuleImportSink {
    /// Must be idempotent for the capsule's canonical identity, revision, and digest.
    fn import(&mut self, capsule: CapsuleEnvelope) -> LegacyMigrationResult<ImportDisposition>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportDisposition {
    Inserted,
    AlreadyPresent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportReport {
    pub inserted: u64,
    pub already_present: u64,
    pub checkpoint: CapsuleExportCheckpoint,
}

#[derive(Debug, Error)]
pub enum LegacyMigrationError {
    #[error("invalid legacy migration input: {0}")]
    Invalid(String),
    #[error("legacy record has no reviewed transform: family={family}, key={key}")]
    Unmapped { family: String, key: String },
    #[error("legacy storage failed: {0}")]
    Storage(String),
    #[error("canonical capsule stream failed: {0}")]
    Contract(String),
}

pub type LegacyMigrationResult<T> = Result<T, LegacyMigrationError>;

#[cfg(test)]
mod tests;
