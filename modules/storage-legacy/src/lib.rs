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
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use thiserror::Error;

pub const DEFAULT_MAX_EXPORT_RECORDS: usize = 1_000_000;
pub const DEFAULT_MAX_EXPORT_BYTES: usize = 8 * 1024 * 1024 * 1024;
pub const DEFAULT_IMPORT_BATCH: usize = 1_000;
pub const DEFAULT_MAX_LEGACY_FILE_BYTES: usize = 1024 * 1024 * 1024;

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
}

impl LegacySnapshotGraph {
    pub fn assemble(records: Vec<LegacyPhysicalRecord>) -> LegacyMigrationResult<Self> {
        let mut graph = Self::default();
        for record in records {
            let key = String::from_utf8(record.key).map_err(|_| {
                LegacyMigrationError::Invalid(
                    "legacy application RocksDB contains a non-UTF-8 key".to_owned(),
                )
            })?;
            if let Some(rest) = key.strip_prefix("obj::") {
                let (family, object_and_column) = rest.split_once("::").ok_or_else(|| {
                    LegacyMigrationError::Invalid(format!("malformed legacy object key {key}"))
                })?;
                let (object_id, column) = object_and_column.rsplit_once("::").ok_or_else(|| {
                    LegacyMigrationError::Invalid(format!("malformed legacy object key {key}"))
                })?;
                if family.is_empty() || object_id.is_empty() || column.is_empty() {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "malformed legacy object key {key}"
                    )));
                }
                let columns = graph
                    .objects
                    .entry((family.to_owned(), object_id.to_owned()))
                    .or_default();
                if columns.insert(column.to_owned(), record.value).is_some() {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "duplicate legacy object column {key}"
                    )));
                }
            } else if let Some(link) = key.strip_prefix("link::") {
                if graph.links.insert(link.to_owned(), record.value).is_some() {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "duplicate legacy link {key}"
                    )));
                }
            } else if let Some(index) = key.strip_prefix("index::") {
                if graph
                    .indexes
                    .insert(index.to_owned(), record.value)
                    .is_some()
                {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "duplicate legacy index {key}"
                    )));
                }
            } else {
                return Err(LegacyMigrationError::Unmapped {
                    family: record.family,
                    key,
                });
            }
        }
        Ok(graph)
    }

    /// Transform every currently reviewed typed family in deterministic order.
    /// Presence of an unreviewed typed family is a hard error.
    pub fn transform_reviewed(
        &self,
        migration_time_micros: i64,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        self.transform_reviewed_with_evidence(
            migration_time_micros,
            &LegacyTransformEvidence::default(),
        )
    }

    pub fn transform_reviewed_with_file_artifacts(
        &self,
        migration_time_micros: i64,
        file_artifacts: &BTreeMap<String, LegacyFileArtifactEvidence>,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        self.transform_reviewed_with_evidence(
            migration_time_micros,
            &LegacyTransformEvidence {
                file_artifacts: file_artifacts.clone(),
                finance: None,
            },
        )
    }

    pub fn transform_reviewed_with_evidence(
        &self,
        migration_time_micros: i64,
        evidence: &LegacyTransformEvidence,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let mut capsules = Vec::new();
        for ((family, legacy_id), columns) in &self.objects {
            let transformed = match family.as_str() {
                "Program" => vec![transform_legacy_program(
                    legacy_id,
                    columns,
                    migration_time_micros,
                )?],
                "Entity" => {
                    let program_id = required_utf8_column("Entity", columns, "programId")?;
                    let owner = self.resolve_program_creature(&program_id)?;
                    vec![transform_legacy_entity(
                        legacy_id,
                        columns,
                        &owner,
                        migration_time_micros,
                    )?]
                }
                "Store" => {
                    let creator = self.resolve_store_creator(legacy_id)?;
                    vec![transform_legacy_store(
                        legacy_id,
                        columns,
                        &creator,
                        migration_time_micros,
                    )?]
                }
                "Chain" => {
                    let store_id = required_utf8_column("Chain", columns, "storeId")?;
                    let creator = self.resolve_store_creator(&store_id)?;
                    vec![transform_legacy_chain(
                        legacy_id,
                        columns,
                        &creator,
                        migration_time_micros,
                    )?]
                }
                "ChainShard" => {
                    let chain_id = required_utf8_column("ChainShard", columns, "workChainId")?;
                    let chain = self.object("Chain", &chain_id)?;
                    let store_id = required_utf8_column("Chain", chain, "storeId")?;
                    let creator = self.resolve_store_creator(&store_id)?;
                    vec![transform_legacy_chain_shard(
                        legacy_id,
                        columns,
                        &creator,
                        migration_time_micros,
                    )?]
                }
                "Session" => {
                    let subject = required_utf8_column("Session", columns, "userId")?;
                    let user = self.resolve_user_for_creature(&subject)?;
                    vec![transform_legacy_session_revocation(
                        legacy_id,
                        columns,
                        &user,
                        migration_time_micros,
                    )?]
                }
                "File" => {
                    let artifact = evidence.file_artifacts.get(legacy_id).ok_or_else(|| {
                        LegacyMigrationError::Unmapped {
                            family: "File.artifact".to_owned(),
                            key: legacy_id.clone(),
                        }
                    })?;
                    let subject = required_utf8_column("File", columns, "ownerId")?;
                    let user = self.resolve_user_for_creature(&subject)?;
                    vec![transform_legacy_file(
                        legacy_id,
                        columns,
                        artifact,
                        &user,
                        migration_time_micros,
                    )?]
                }
                "Creature" => {
                    let finance = evidence.finance.as_ref().ok_or_else(|| {
                        LegacyMigrationError::Unmapped {
                            family: "Creature.finance_config".to_owned(),
                            key: legacy_id.clone(),
                        }
                    })?;
                    let email = self.optional_link_utf8(&format!("UserIdToEmail::{legacy_id}"))?;
                    let creature_type = required_utf8_column("Creature", columns, "type")?;
                    let owner_user_id = if creature_type == "human" {
                        legacy_id.clone()
                    } else {
                        let owner = required_utf8_column("Creature", columns, "ownerId")?;
                        let owner_columns = self.object("Creature", &owner)?;
                        if required_utf8_column("Creature", owner_columns, "type")? != "human" {
                            return Err(LegacyMigrationError::Invalid(format!(
                                "legacy Creature {legacy_id} owner is not human"
                            )));
                        }
                        owner
                    };
                    transform_legacy_creature(
                        legacy_id,
                        columns,
                        &owner_user_id,
                        email.as_deref(),
                        finance,
                        migration_time_micros,
                    )?
                }
                _ => {
                    return Err(LegacyMigrationError::Unmapped {
                        family: family.clone(),
                        key: legacy_id.clone(),
                    });
                }
            };
            capsules.extend(transformed);
        }
        capsules.sort_by(|left, right| {
            left.kind
                .0
                .cmp(&right.kind.0)
                .then_with(|| left.id.0.cmp(&right.id.0))
        });
        Ok(capsules)
    }

    fn object(
        &self,
        family: &str,
        legacy_id: &str,
    ) -> LegacyMigrationResult<&BTreeMap<String, Vec<u8>>> {
        self.objects
            .get(&(family.to_owned(), legacy_id.to_owned()))
            .ok_or_else(|| {
                LegacyMigrationError::Invalid(format!("legacy graph omits {family} {legacy_id}"))
            })
    }

    fn resolve_program_creature(&self, program_id: &str) -> LegacyMigrationResult<String> {
        required_utf8_column("Program", self.object("Program", program_id)?, "machineId")
    }

    fn resolve_user_for_creature(&self, creature_id: &str) -> LegacyMigrationResult<String> {
        let columns = self.object("Creature", creature_id)?;
        if required_utf8_column("Creature", columns, "type")? == "human" {
            return Ok(creature_id.to_owned());
        }
        let owner = required_utf8_column("Creature", columns, "ownerId")?;
        let owner_columns = self.object("Creature", &owner)?;
        if required_utf8_column("Creature", owner_columns, "type")? != "human" {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy Creature {creature_id} owner is not human"
            )));
        }
        Ok(owner)
    }

    fn resolve_store_creator(&self, store_id: &str) -> LegacyMigrationResult<String> {
        let suffix = format!("::{store_id}");
        let mut creators = self.links.keys().filter_map(|link| {
            link.strip_prefix("creatorof::")
                .and_then(|candidate| candidate.strip_suffix(&suffix))
                .filter(|creator| !creator.is_empty())
        });
        let creator = creators.next().ok_or_else(|| {
            LegacyMigrationError::Invalid(format!("legacy Store {store_id} has no creator link"))
        })?;
        if creators.next().is_some() {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy Store {store_id} has multiple creator links"
            )));
        }
        Ok(creator.to_owned())
    }

    fn optional_link_utf8(&self, key: &str) -> LegacyMigrationResult<Option<String>> {
        self.links
            .get(key)
            .map(|value| {
                String::from_utf8(value.clone()).map_err(|_| {
                    LegacyMigrationError::Invalid(format!("legacy link {key} is not UTF-8"))
                })
            })
            .transpose()
    }
}

/// Buffers one bounded snapshot so relationships can be resolved before capsule emission.
pub struct LegacyTypedGraphTransformer {
    manifest_digest: [u8; 32],
    migration_time_micros: i64,
    records: Vec<LegacyPhysicalRecord>,
    evidence: LegacyTransformEvidence,
}

impl LegacyTypedGraphTransformer {
    #[must_use]
    pub fn new(manifest_digest: [u8; 32], migration_time_micros: i64) -> Self {
        Self {
            manifest_digest,
            migration_time_micros,
            records: Vec::new(),
            evidence: LegacyTransformEvidence::default(),
        }
    }

    #[must_use]
    pub fn with_file_artifacts(
        mut self,
        file_artifacts: BTreeMap<String, LegacyFileArtifactEvidence>,
    ) -> Self {
        self.evidence.file_artifacts = file_artifacts;
        self
    }

    #[must_use]
    pub fn with_finance_config(mut self, finance: LegacyFinanceConfig) -> Self {
        self.evidence.finance = Some(finance);
        self
    }
}

impl LegacyTransformer for LegacyTypedGraphTransformer {
    fn manifest_digest(&self) -> [u8; 32] {
        self.manifest_digest
    }

    fn transform(
        &mut self,
        record: LegacyPhysicalRecord,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        self.records.push(record);
        Ok(Vec::new())
    }

    fn finish(&mut self) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let records = std::mem::take(&mut self.records);
        LegacySnapshotGraph::assemble(records)?
            .transform_reviewed_with_evidence(self.migration_time_micros, &self.evidence)
    }
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

pub struct RocksDbLegacySource {
    database: DB,
    snapshot_id: String,
}

impl RocksDbLegacySource {
    pub fn open_read_only(path: &Path, snapshot_id: &str) -> LegacyMigrationResult<Self> {
        if snapshot_id.is_empty() || snapshot_id.len() > 256 {
            return Err(LegacyMigrationError::Invalid(
                "legacy snapshot ID is empty or too long".to_owned(),
            ));
        }
        let mut options = Options::default();
        options.create_if_missing(false);
        let database = DB::open_for_read_only(&options, path, false)
            .map_err(|error| LegacyMigrationError::Storage(error.to_string()))?;
        Ok(Self {
            database,
            snapshot_id: snapshot_id.to_owned(),
        })
    }
}

impl LegacyRecordSource for RocksDbLegacySource {
    fn read_snapshot(
        &self,
        max_records: usize,
        max_bytes: usize,
    ) -> LegacyMigrationResult<LegacySnapshot> {
        if max_records == 0 || max_bytes == 0 {
            return Err(LegacyMigrationError::Invalid(
                "legacy snapshot bounds must be positive".to_owned(),
            ));
        }
        let snapshot = self.database.snapshot();
        let mut records = Vec::new();
        let mut bytes = 0usize;
        for item in snapshot.iterator(IteratorMode::Start) {
            let (key, value) =
                item.map_err(|error| LegacyMigrationError::Storage(error.to_string()))?;
            if records.len() >= max_records {
                return Err(LegacyMigrationError::Invalid(
                    "legacy snapshot exceeds the record bound".to_owned(),
                ));
            }
            bytes = bytes
                .checked_add(key.len())
                .and_then(|value_bytes| value_bytes.checked_add(value.len()))
                .ok_or_else(|| {
                    LegacyMigrationError::Invalid("legacy snapshot size overflow".to_owned())
                })?;
            if bytes > max_bytes {
                return Err(LegacyMigrationError::Invalid(
                    "legacy snapshot exceeds the byte bound".to_owned(),
                ));
            }
            records.push(LegacyPhysicalRecord {
                family: "application-rocksdb-default".to_owned(),
                key: key.to_vec(),
                value: value.to_vec(),
            });
        }
        Ok(LegacySnapshot {
            snapshot_id: self.snapshot_id.clone(),
            records,
        })
    }
}

pub fn export_canonical(
    source_provider: &str,
    created_at_micros: i64,
    source: &impl LegacyRecordSource,
    transformer: &mut impl LegacyTransformer,
    max_records: usize,
    max_bytes: usize,
) -> LegacyMigrationResult<CanonicalCapsuleExport> {
    let snapshot = source.read_snapshot(max_records, max_bytes)?;
    let mut capsules = Vec::new();
    for record in snapshot.records {
        capsules.extend(transformer.transform(record)?);
    }
    capsules.extend(transformer.finish()?);
    capsules.sort_by(|left, right| {
        left.kind
            .0
            .cmp(&right.kind.0)
            .then_with(|| left.id.0.cmp(&right.id.0))
            .then_with(|| left.revision.cmp(&right.revision))
    });
    let header = CapsuleExportHeader {
        format_version: 1,
        source_provider: source_provider.to_owned(),
        source_snapshot_id: snapshot.snapshot_id,
        transform_manifest_digest: transformer.manifest_digest().to_vec(),
        created_at_micros,
    };
    CanonicalCapsuleExport::build(header, capsules)
        .map_err(|error| LegacyMigrationError::Contract(error.to_string()))
}

pub fn import_canonical(
    export: &CanonicalCapsuleExport,
    resume: Option<&CapsuleExportCheckpoint>,
    sink: &mut impl CapsuleImportSink,
    max_batch: usize,
) -> LegacyMigrationResult<ImportReport> {
    if max_batch == 0 || max_batch > DEFAULT_IMPORT_BATCH {
        return Err(LegacyMigrationError::Invalid(
            "import batch is outside the supported bound".to_owned(),
        ));
    }
    export
        .validate()
        .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?;
    let start = if let Some(checkpoint) = resume {
        let expected = export
            .checkpoint(checkpoint.next_sequence)
            .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?;
        if &expected != checkpoint {
            return Err(LegacyMigrationError::Invalid(
                "resume checkpoint is not bound to this export".to_owned(),
            ));
        }
        usize::try_from(checkpoint.next_sequence).map_err(|_| {
            LegacyMigrationError::Invalid("resume checkpoint sequence overflow".to_owned())
        })?
    } else {
        0
    };
    let end = start.saturating_add(max_batch).min(export.records.len());
    let mut inserted = 0u64;
    let mut already_present = 0u64;
    for record in &export.records[start..end] {
        let capsule = record
            .decode()
            .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?;
        match sink.import(capsule)? {
            ImportDisposition::Inserted => inserted += 1,
            ImportDisposition::AlreadyPresent => already_present += 1,
        }
    }
    let checkpoint = export
        .checkpoint(u64::try_from(end).map_err(|_| {
            LegacyMigrationError::Invalid("import checkpoint sequence overflow".to_owned())
        })?)
        .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?;
    Ok(ImportReport {
        inserted,
        already_present,
        checkpoint,
    })
}

#[must_use]
pub fn deterministic_legacy_capsule_id(family: &str, legacy_id: &[u8]) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(b"ASEMAN-LEGACY-CAPSULE-ID-V1\0");
    hasher.update((family.len() as u64).to_be_bytes());
    hasher.update(family.as_bytes());
    hasher.update((legacy_id.len() as u64).to_be_bytes());
    hasher.update(legacy_id);
    hasher.finalize()[..16]
        .try_into()
        .expect("fixed digest slice")
}

/// Fixture-backed transform for the self-contained legacy `Program` object family.
pub fn transform_legacy_program(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    if legacy_id.is_empty() {
        return Err(LegacyMigrationError::Invalid(
            "legacy Program ID is empty".to_owned(),
        ));
    }
    let allowed = ["|", "id", "machineId", "runtime", "path", "comment"];
    if columns
        .keys()
        .any(|column| !allowed.contains(&column.as_str()))
    {
        return Err(LegacyMigrationError::Invalid(
            "legacy Program contains an unreviewed column".to_owned(),
        ));
    }
    if let Some(stored_id) = columns.get("id") {
        let stored_id = utf8_program_column("id", stored_id)?;
        if stored_id != legacy_id {
            return Err(LegacyMigrationError::Invalid(
                "legacy Program key and stored ID disagree".to_owned(),
            ));
        }
    }
    let creature = required_program_column(columns, "machineId")?;
    let runtime = required_program_column(columns, "runtime")?;
    let path = required_program_column(columns, "path")?;
    let comment = columns
        .get("comment")
        .map(|value| utf8_program_column("comment", value))
        .transpose()?
        .unwrap_or_default();
    let creature_id = deterministic_legacy_capsule_id("Creature", creature.as_bytes());
    let capsule = CapsuleEnvelope {
        encoding_version: 1,
        id: CapsuleId(deterministic_legacy_capsule_id(
            "Program",
            legacy_id.as_bytes(),
        )),
        kind: CapsuleKind("core.program".to_owned()),
        storage_class: StorageClass::Core,
        owner_scope: OwnerScope::Creature(creature_id),
        schema_version: 1,
        revision: 1,
        created_at_micros: migration_time_micros,
        updated_at_micros: migration_time_micros,
        previous_integrity: None,
        integrity_hash: CapsuleDigest {
            algorithm: "sha2-256".to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: false,
        relationships: vec![CapsuleRelationship {
            name: "creature".to_owned(),
            target_kind: CapsuleKind("core.creature".to_owned()),
            target_id: CapsuleId(creature_id),
        }],
        body: Some(CapsuleValue::Object(BTreeMap::from([
            ("machine_id".to_owned(), CapsuleValue::Text(creature)),
            ("runtime".to_owned(), CapsuleValue::Text(runtime)),
            ("path".to_owned(), CapsuleValue::Text(path)),
            ("comment".to_owned(), CapsuleValue::Text(comment)),
        ]))),
    };
    capsule
        .seal()
        .map_err(|error| LegacyMigrationError::Contract(error.to_string()))
}

fn required_program_column(
    columns: &BTreeMap<String, Vec<u8>>,
    name: &str,
) -> LegacyMigrationResult<String> {
    let value = columns
        .get(name)
        .ok_or_else(|| LegacyMigrationError::Invalid(format!("legacy Program omits {name}")))?;
    let value = utf8_program_column(name, value)?;
    if value.is_empty() {
        return Err(LegacyMigrationError::Invalid(format!(
            "legacy Program has empty {name}"
        )));
    }
    Ok(value)
}

fn utf8_program_column(name: &str, value: &[u8]) -> LegacyMigrationResult<String> {
    String::from_utf8(value.to_vec())
        .map_err(|_| LegacyMigrationError::Invalid(format!("legacy Program {name} is not UTF-8")))
}

/// One row from the legacy QuestDB `buildlogs` table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyBuildLogRow {
    pub id: String,
    pub build_id: String,
    pub machine_id: String,
    pub vm_id: String,
    pub log_type: String,
    pub data: String,
    /// The legacy writer records Unix milliseconds despite the generic column name.
    pub time_millis: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacySignalRow {
    pub id: String,
    pub store_id: String,
    pub user_id: String,
    pub data: String,
    pub encoded_tags: String,
    pub time_millis: i64,
    pub edited: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacySignalStreamPolicy {
    pub authorization_scope: Vec<u8>,
    pub retention_class: String,
}

/// Transform the mutable QuestDB signal-history table into ordered immutable streams.
pub fn transform_legacy_signal_rows(
    mut rows: Vec<LegacySignalRow>,
    policies: &BTreeMap<String, LegacySignalStreamPolicy>,
) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
    rows.sort_by(|left, right| {
        left.store_id
            .cmp(&right.store_id)
            .then_with(|| left.time_millis.cmp(&right.time_millis))
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut sequences = BTreeMap::<String, i64>::new();
    let mut capsules = Vec::with_capacity(rows.len());
    for row in rows {
        for (name, value) in [
            ("id", row.id.as_str()),
            ("store_id", row.store_id.as_str()),
            ("user_id", row.user_id.as_str()),
        ] {
            if value.is_empty() {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy signal row has empty {name}"
                )));
            }
        }
        if row.time_millis <= 0 {
            return Err(LegacyMigrationError::Invalid(
                "legacy signal timestamp is not positive".to_owned(),
            ));
        }
        let occurred_at_micros = row.time_millis.checked_mul(1_000).ok_or_else(|| {
            LegacyMigrationError::Invalid("legacy signal timestamp overflows micros".to_owned())
        })?;
        let tags = decode_legacy_tags(&row.encoded_tags)?;
        let policy = policies.get(&row.store_id).ok_or_else(|| {
            LegacyMigrationError::Invalid(format!(
                "legacy signal store {} has no resolved authorization/retention policy",
                row.store_id
            ))
        })?;
        if policy.authorization_scope.is_empty() || policy.retention_class.is_empty() {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy signal store {} has incomplete stream policy",
                row.store_id
            )));
        }
        let sequence = sequences.entry(row.store_id.clone()).or_default();
        *sequence = sequence.checked_add(1).ok_or_else(|| {
            LegacyMigrationError::Invalid("legacy signal sequence overflow".to_owned())
        })?;
        let payload = encode_value(&CapsuleValue::Object(BTreeMap::from([
            ("legacy_id".to_owned(), CapsuleValue::Text(row.id.clone())),
            (
                "store_id".to_owned(),
                CapsuleValue::Text(row.store_id.clone()),
            ),
            (
                "user_id".to_owned(),
                CapsuleValue::Text(row.user_id.clone()),
            ),
            ("data".to_owned(), CapsuleValue::Text(row.data)),
            (
                "tags".to_owned(),
                CapsuleValue::Array(tags.into_iter().map(CapsuleValue::Text).collect()),
            ),
            ("edited".to_owned(), CapsuleValue::Bool(row.edited)),
        ])))
        .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?;
        let payload_digest = Sha256::digest(&payload).to_vec();
        let trace_id = digest_hex(b"ASEMAN-LEGACY-SIGNAL-TRACE-V1\0", row.id.as_bytes());
        let stream_id = format!("store:{}", row.store_id);
        let capsule = CapsuleEnvelope {
            encoding_version: 1,
            id: CapsuleId(deterministic_legacy_capsule_id(
                "QuestDB.storage",
                row.id.as_bytes(),
            )),
            kind: CapsuleKind("realtime.event".to_owned()),
            storage_class: StorageClass::Realtime,
            owner_scope: OwnerScope::Global,
            schema_version: 1,
            revision: 1,
            created_at_micros: occurred_at_micros,
            updated_at_micros: occurred_at_micros,
            previous_integrity: None,
            integrity_hash: CapsuleDigest {
                algorithm: "sha2-256".to_owned(),
                bytes: vec![0; 32],
            },
            tombstone: false,
            relationships: Vec::new(),
            body: Some(CapsuleValue::Object(BTreeMap::from([
                ("stream_id".to_owned(), CapsuleValue::Text(stream_id)),
                ("sequence".to_owned(), CapsuleValue::Integer(*sequence)),
                (
                    "event_type".to_owned(),
                    CapsuleValue::Text("legacy.store.signal".to_owned()),
                ),
                (
                    "authorization_scope".to_owned(),
                    CapsuleValue::Bytes(policy.authorization_scope.clone()),
                ),
                ("producer".to_owned(), CapsuleValue::Text(row.user_id)),
                ("trace_id".to_owned(), CapsuleValue::Text(trace_id)),
                (
                    "correlation_id".to_owned(),
                    CapsuleValue::Text(String::new()),
                ),
                ("payload".to_owned(), CapsuleValue::Bytes(payload)),
                (
                    "payload_digest".to_owned(),
                    CapsuleValue::Bytes(payload_digest),
                ),
                (
                    "retention_class".to_owned(),
                    CapsuleValue::Text(policy.retention_class.clone()),
                ),
                ("idempotency_key".to_owned(), CapsuleValue::Text(row.id)),
                (
                    "occurred_at_micros".to_owned(),
                    CapsuleValue::Integer(occurred_at_micros),
                ),
            ]))),
        }
        .seal()
        .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?;
        capsules.push(capsule);
    }
    Ok(capsules)
}

fn decode_legacy_tags(encoded: &str) -> LegacyMigrationResult<Vec<String>> {
    if encoded.is_empty() {
        return Ok(Vec::new());
    }
    if !encoded.starts_with('|') || !encoded.ends_with('|') {
        return Err(LegacyMigrationError::Invalid(
            "legacy signal tags are not delimiter framed".to_owned(),
        ));
    }
    let tags = encoded[1..encoded.len() - 1]
        .split('|')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if tags.len() > 24
        || tags.iter().any(|tag| {
            tag.is_empty()
                || tag.len() > 128
                || tag.chars().any(|character| {
                    !(character.is_ascii_alphanumeric()
                        || matches!(character, '=' | '@' | '.' | ':' | '-' | '/' | '+' | '#'))
                })
        })
    {
        return Err(LegacyMigrationError::Invalid(
            "legacy signal tags violate the bounded tag contract".to_owned(),
        ));
    }
    Ok(tags)
}

fn digest_hex(domain: &[u8], value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
    digest
        .finalize()
        .iter()
        .flat_map(|byte| {
            [
                char::from(HEX[usize::from(byte >> 4)]),
                char::from(HEX[usize::from(byte & 0x0f)]),
            ]
        })
        .collect()
}

/// Transform a legacy build-log row after the exporter resolves its VM to a creature.
///
/// `resolved_creature_legacy_id` is supplied by the server-side snapshot graph. The row
/// cannot select a different owner: a non-empty legacy `machine_id` must agree with that
/// binding. This preserves the tenancy rule used by the target storage providers.
pub fn transform_legacy_build_log(
    row: &LegacyBuildLogRow,
    resolved_creature_legacy_id: &str,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    for (name, value) in [
        ("id", row.id.as_str()),
        ("vm_id", row.vm_id.as_str()),
        ("log_type", row.log_type.as_str()),
        ("resolved_creature_legacy_id", resolved_creature_legacy_id),
    ] {
        if value.is_empty() {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy build log has empty {name}"
            )));
        }
    }
    if !row.machine_id.is_empty() && row.machine_id != resolved_creature_legacy_id {
        return Err(LegacyMigrationError::Invalid(
            "legacy build log machine ID disagrees with the resolved VM owner".to_owned(),
        ));
    }
    if row.time_millis <= 0 {
        return Err(LegacyMigrationError::Invalid(
            "legacy build log timestamp is not positive".to_owned(),
        ));
    }
    let observed_at_micros = row.time_millis.checked_mul(1_000).ok_or_else(|| {
        LegacyMigrationError::Invalid("legacy build log timestamp overflows micros".to_owned())
    })?;
    let creature_id =
        deterministic_legacy_capsule_id("Creature", resolved_creature_legacy_id.as_bytes());
    CapsuleEnvelope {
        encoding_version: 1,
        id: CapsuleId(deterministic_legacy_capsule_id(
            "QuestDB.buildlogs",
            row.id.as_bytes(),
        )),
        kind: CapsuleKind("telemetry.build_log".to_owned()),
        storage_class: StorageClass::Telemetry,
        owner_scope: OwnerScope::Creature(creature_id),
        schema_version: 1,
        revision: 1,
        created_at_micros: observed_at_micros,
        updated_at_micros: observed_at_micros,
        previous_integrity: None,
        integrity_hash: CapsuleDigest {
            algorithm: "sha2-256".to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: false,
        relationships: Vec::new(),
        body: Some(CapsuleValue::Object(BTreeMap::from([
            ("legacy_id".to_owned(), CapsuleValue::Text(row.id.clone())),
            (
                "build_id".to_owned(),
                CapsuleValue::Text(row.build_id.clone()),
            ),
            (
                "machine_id".to_owned(),
                CapsuleValue::Text(resolved_creature_legacy_id.to_owned()),
            ),
            (
                "workload_id".to_owned(),
                CapsuleValue::Text(row.vm_id.clone()),
            ),
            (
                "log_type".to_owned(),
                CapsuleValue::Text(row.log_type.clone()),
            ),
            ("message".to_owned(), CapsuleValue::Text(row.data.clone())),
            (
                "observed_at_micros".to_owned(),
                CapsuleValue::Integer(observed_at_micros),
            ),
        ]))),
    }
    .seal()
    .map_err(|error| LegacyMigrationError::Contract(error.to_string()))
}

/// Fixture-backed transform for a legacy `Entity` after resolving its program owner.
pub fn transform_legacy_entity(
    legacy_key: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    resolved_creature_legacy_id: &str,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    validate_columns(
        "Entity",
        columns,
        &["|", "programId", "entityId", "entityType", "imageName"],
    )?;
    let program_id = required_utf8_column("Entity", columns, "programId")?;
    let entity_id = required_utf8_column("Entity", columns, "entityId")?;
    let entity_type = required_utf8_column("Entity", columns, "entityType")?;
    let image_name = optional_utf8_column("Entity", columns, "imageName")?;
    if legacy_key != format!("{program_id}::{entity_id}") {
        return Err(LegacyMigrationError::Invalid(
            "legacy Entity key disagrees with its program and entity IDs".to_owned(),
        ));
    }
    let creature_id = required_resolved_creature("Entity", resolved_creature_legacy_id)?;
    let program_capsule_id = deterministic_legacy_capsule_id("Program", program_id.as_bytes());
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "Entity",
            kind: "core.entity",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Creature(creature_id),
            migration_time_micros,
        },
        legacy_key,
        vec![CapsuleRelationship {
            name: "program".to_owned(),
            target_kind: CapsuleKind("core.program".to_owned()),
            target_id: CapsuleId(program_capsule_id),
        }],
        BTreeMap::from([
            ("entity_name".to_owned(), CapsuleValue::Text(entity_id)),
            ("entity_type".to_owned(), CapsuleValue::Text(entity_type)),
            ("image_name".to_owned(), CapsuleValue::Text(image_name)),
        ]),
    )
}

/// Fixture-backed transform for a legacy `Store` after resolving its creator link.
pub fn transform_legacy_store(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    resolved_creator_legacy_id: &str,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    if legacy_id.is_empty() {
        return Err(LegacyMigrationError::Invalid(
            "legacy Store ID is empty".to_owned(),
        ));
    }
    validate_columns(
        "Store",
        columns,
        &[
            "|",
            "tag",
            "parentId",
            "isPublic",
            "persHist",
            "memberCount",
            "signalCount",
        ],
    )?;
    let tag = optional_utf8_column("Store", columns, "tag")?;
    let parent_id = optional_utf8_column("Store", columns, "parentId")?;
    let is_public = required_bool_column("Store", columns, "isPublic")?;
    let persistent_history = required_bool_column("Store", columns, "persHist")?;
    let member_count = i64::from(required_i32_le_column("Store", columns, "memberCount")?);
    let signal_count = required_i64_le_column("Store", columns, "signalCount")?;
    if member_count < 0 || signal_count < 0 {
        return Err(LegacyMigrationError::Invalid(
            "legacy Store contains a negative count".to_owned(),
        ));
    }
    let creature_id = required_resolved_creature("Store", resolved_creator_legacy_id)?;
    let mut relationships = vec![CapsuleRelationship {
        name: "creature".to_owned(),
        target_kind: CapsuleKind("core.creature".to_owned()),
        target_id: CapsuleId(creature_id),
    }];
    if !parent_id.is_empty() {
        if parent_id == legacy_id {
            return Err(LegacyMigrationError::Invalid(
                "legacy Store cannot be its own parent".to_owned(),
            ));
        }
        relationships.push(CapsuleRelationship {
            name: "parent".to_owned(),
            target_kind: CapsuleKind("core.store".to_owned()),
            target_id: CapsuleId(deterministic_legacy_capsule_id(
                "Store",
                parent_id.as_bytes(),
            )),
        });
    }
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "Store",
            kind: "core.store",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Creature(creature_id),
            migration_time_micros,
        },
        legacy_id,
        relationships,
        BTreeMap::from([
            ("is_public".to_owned(), CapsuleValue::Bool(is_public)),
            (
                "member_count".to_owned(),
                CapsuleValue::Integer(member_count),
            ),
            (
                "persistent_history".to_owned(),
                CapsuleValue::Bool(persistent_history),
            ),
            (
                "signal_count".to_owned(),
                CapsuleValue::Integer(signal_count),
            ),
            ("tag".to_owned(), CapsuleValue::Text(tag)),
        ]),
    )
}

/// Fixture-backed transform for a legacy work chain after resolving its store owner.
pub fn transform_legacy_chain(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    resolved_creature_legacy_id: &str,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    validate_columns("Chain", columns, &["|", "id", "storeId"])?;
    let stored_id = required_utf8_column("Chain", columns, "id")?;
    if stored_id != legacy_id {
        return Err(LegacyMigrationError::Invalid(
            "legacy Chain key and stored ID disagree".to_owned(),
        ));
    }
    let store_id = required_utf8_column("Chain", columns, "storeId")?;
    let creature_id = required_resolved_creature("Chain", resolved_creature_legacy_id)?;
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "Chain",
            kind: "core.chain",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Creature(creature_id),
            migration_time_micros,
        },
        legacy_id,
        vec![CapsuleRelationship {
            name: "store".to_owned(),
            target_kind: CapsuleKind("core.store".to_owned()),
            target_id: CapsuleId(deterministic_legacy_capsule_id(
                "Store",
                store_id.as_bytes(),
            )),
        }],
        BTreeMap::from([
            ("store_id".to_owned(), CapsuleValue::Text(store_id)),
            ("status".to_owned(), CapsuleValue::Text("active".to_owned())),
        ]),
    )
}

/// Fixture-backed transform for a named legacy shard after resolving its chain owner.
pub fn transform_legacy_chain_shard(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    resolved_creature_legacy_id: &str,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    validate_columns("ChainShard", columns, &["|", "id", "workChainId"])?;
    let stored_id = required_utf8_column("ChainShard", columns, "id")?;
    if stored_id != legacy_id {
        return Err(LegacyMigrationError::Invalid(
            "legacy ChainShard key and stored ID disagree".to_owned(),
        ));
    }
    let chain_id = required_utf8_column("ChainShard", columns, "workChainId")?;
    let creature_id = required_resolved_creature("ChainShard", resolved_creature_legacy_id)?;
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "ChainShard",
            kind: "core.chain_shard",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Creature(creature_id),
            migration_time_micros,
        },
        legacy_id,
        vec![CapsuleRelationship {
            name: "chain".to_owned(),
            target_kind: CapsuleKind("core.chain".to_owned()),
            target_id: CapsuleId(deterministic_legacy_capsule_id(
                "Chain",
                chain_id.as_bytes(),
            )),
        }],
        BTreeMap::from([
            ("work_chain_id".to_owned(), CapsuleValue::Text(chain_id)),
            (
                "shard_name".to_owned(),
                CapsuleValue::Text(legacy_id.to_owned()),
            ),
        ]),
    )
}

/// Convert a legacy bearer session into a target revocation marker.
///
/// Legacy sessions have no trustworthy issue or expiry timestamps. They are never made
/// live in the target: the digest is retained with zero unknown timestamps and an
/// explicit migration-time revocation so rollback/cutover checks can deny replay.
pub fn transform_legacy_session_revocation(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    resolved_user_legacy_id: &str,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    validate_columns("Session", columns, &["|", "userId"])?;
    let _subject_creature_id = required_utf8_column("Session", columns, "userId")?;
    if legacy_id.is_empty() || resolved_user_legacy_id.is_empty() {
        return Err(LegacyMigrationError::Invalid(
            "legacy Session ID or resolved target user is empty".to_owned(),
        ));
    }
    let mut digest = Sha256::new();
    digest.update(b"ASEMAN-LEGACY-SESSION-REVOCATION-V1\0");
    digest.update((legacy_id.len() as u64).to_be_bytes());
    digest.update(legacy_id.as_bytes());
    let token_digest = digest.finalize().to_vec();
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "Session",
            kind: "core.session",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Global,
            migration_time_micros,
        },
        legacy_id,
        vec![CapsuleRelationship {
            name: "user".to_owned(),
            target_kind: CapsuleKind("core.user".to_owned()),
            target_id: CapsuleId(deterministic_legacy_capsule_id(
                "User",
                resolved_user_legacy_id.as_bytes(),
            )),
        }],
        BTreeMap::from([
            ("token_digest".to_owned(), CapsuleValue::Bytes(token_digest)),
            ("issued_at_micros".to_owned(), CapsuleValue::Integer(0)),
            ("expires_at_micros".to_owned(), CapsuleValue::Integer(0)),
            (
                "revoked_at_micros".to_owned(),
                CapsuleValue::Integer(migration_time_micros),
            ),
        ]),
    )
}

/// Verified metadata for a legacy filesystem object copied by the migration runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyFileArtifactEvidence {
    pub store_key: String,
    pub content_digest: [u8; 32],
    pub media_type: String,
    pub size_bytes: u64,
}

impl LegacyFileArtifactEvidence {
    pub fn from_bytes(
        store_key: &str,
        media_type: &str,
        content: &[u8],
    ) -> LegacyMigrationResult<Self> {
        if store_key.is_empty() || store_key.contains('\0') {
            return Err(LegacyMigrationError::Invalid(
                "legacy file store key is empty or contains NUL".to_owned(),
            ));
        }
        if content.len() > DEFAULT_MAX_LEGACY_FILE_BYTES {
            return Err(LegacyMigrationError::Invalid(
                "legacy file exceeds the migration byte bound".to_owned(),
            ));
        }
        let media_type = if media_type.is_empty() {
            "application/octet-stream"
        } else {
            media_type
        };
        Ok(Self {
            store_key: store_key.to_owned(),
            content_digest: Sha256::digest(content).into(),
            media_type: media_type.to_owned(),
            size_bytes: u64::try_from(content.len()).map_err(|_| {
                LegacyMigrationError::Invalid("legacy file size overflows u64".to_owned())
            })?,
        })
    }
}

/// Transform legacy file metadata only after its external bytes have digest evidence.
pub fn transform_legacy_file(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    artifact: &LegacyFileArtifactEvidence,
    resolved_owner_user_legacy_id: &str,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    validate_columns("File", columns, &["|", "storeId", "ownerId"])?;
    let owner_creature_id = required_utf8_column("File", columns, "ownerId")?;
    let store_id = optional_utf8_column("File", columns, "storeId")?;
    if artifact.store_key.is_empty()
        || artifact.content_digest == [0; 32]
        || resolved_owner_user_legacy_id.is_empty()
    {
        return Err(LegacyMigrationError::Invalid(
            "legacy File artifact evidence is incomplete".to_owned(),
        ));
    }
    let size_bytes = i64::try_from(artifact.size_bytes).map_err(|_| {
        LegacyMigrationError::Invalid("legacy File size exceeds target integer range".to_owned())
    })?;
    let creature_id = deterministic_legacy_capsule_id("Creature", owner_creature_id.as_bytes());
    let mut relationships = vec![CapsuleRelationship {
        name: "owner".to_owned(),
        target_kind: CapsuleKind("core.user".to_owned()),
        target_id: CapsuleId(deterministic_legacy_capsule_id(
            "User",
            resolved_owner_user_legacy_id.as_bytes(),
        )),
    }];
    if !store_id.is_empty() {
        relationships.push(CapsuleRelationship {
            name: "store".to_owned(),
            target_kind: CapsuleKind("core.store".to_owned()),
            target_id: CapsuleId(deterministic_legacy_capsule_id(
                "Store",
                store_id.as_bytes(),
            )),
        });
    }
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "File",
            kind: "core.file",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Creature(creature_id),
            migration_time_micros,
        },
        legacy_id,
        relationships,
        BTreeMap::from([
            (
                "store_key".to_owned(),
                CapsuleValue::Text(artifact.store_key.clone()),
            ),
            (
                "content_digest".to_owned(),
                CapsuleValue::Bytes(artifact.content_digest.to_vec()),
            ),
            (
                "media_type".to_owned(),
                CapsuleValue::Text(artifact.media_type.clone()),
            ),
            ("size_bytes".to_owned(), CapsuleValue::Integer(size_bytes)),
        ]),
    )
}

/// Convert a legacy RSA SubjectPublicKeyInfo PEM into tagged multicodec bytes.
pub fn encode_legacy_rsa_public_key(public_key_pem: &str) -> LegacyMigrationResult<Vec<u8>> {
    let key = RsaPublicKey::from_public_key_pem(public_key_pem).map_err(|_| {
        LegacyMigrationError::Invalid("legacy Creature publicKey is not RSA SPKI PEM".to_owned())
    })?;
    let der = key.to_public_key_der().map_err(|_| {
        LegacyMigrationError::Invalid("legacy Creature RSA key cannot encode as SPKI".to_owned())
    })?;
    // Multicodec rsa-pub (0x1205), unsigned-varint encoded as 0x85 0x24.
    let mut encoded = Vec::with_capacity(2 + der.as_bytes().len());
    encoded.extend_from_slice(&[0x85, 0x24]);
    encoded.extend_from_slice(der.as_bytes());
    Ok(encoded)
}

/// Split one legacy unified Creature into target identity, boundary, and wallet records.
pub fn transform_legacy_creature(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    resolved_owner_user_legacy_id: &str,
    email: Option<&str>,
    finance: &LegacyFinanceConfig,
    migration_time_micros: i64,
) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
    validate_columns(
        "Creature",
        columns,
        &[
            "|",
            "type",
            "username",
            "publicKey",
            "chainId",
            "subchainId",
            "ownerId",
            "balance",
        ],
    )?;
    if legacy_id.is_empty() || resolved_owner_user_legacy_id.is_empty() {
        return Err(LegacyMigrationError::Invalid(
            "legacy Creature identity or resolved owner is empty".to_owned(),
        ));
    }
    let creature_type = required_utf8_column("Creature", columns, "type")?;
    let username = required_utf8_column("Creature", columns, "username")?;
    let public_key_pem = required_utf8_column("Creature", columns, "publicKey")?;
    let public_key = encode_legacy_rsa_public_key(&public_key_pem)?;
    let chain_id = optional_utf8_column("Creature", columns, "chainId")?;
    let subchain_id = optional_utf8_column("Creature", columns, "subchainId")?;
    let balance = required_i64_le_column("Creature", columns, "balance")?;
    if finance.currency.is_empty()
        || finance.currency.len() > 16
        || !finance
            .currency
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        || finance.scale > 18
    {
        return Err(LegacyMigrationError::Invalid(
            "legacy finance currency/scale configuration is invalid".to_owned(),
        ));
    }
    let creature_id = deterministic_legacy_capsule_id("Creature", legacy_id.as_bytes());
    let owner_user_id =
        deterministic_legacy_capsule_id("User", resolved_owner_user_legacy_id.as_bytes());
    let mut capsules = Vec::new();
    if creature_type == "human" {
        if resolved_owner_user_legacy_id != legacy_id {
            return Err(LegacyMigrationError::Invalid(
                "legacy human Creature must resolve to its own target user".to_owned(),
            ));
        }
        capsules.push(seal_legacy_capsule(
            LegacyCapsuleSpec {
                family: "User",
                kind: "core.user",
                storage_class: StorageClass::Core,
                owner_scope: OwnerScope::Global,
                migration_time_micros,
            },
            legacy_id,
            Vec::new(),
            BTreeMap::from([
                ("username".to_owned(), CapsuleValue::Text(username.clone())),
                (
                    "email".to_owned(),
                    CapsuleValue::Text(email.unwrap_or_default().to_owned()),
                ),
                (
                    "public_key".to_owned(),
                    CapsuleValue::Bytes(public_key.clone()),
                ),
                ("status".to_owned(), CapsuleValue::Text("active".to_owned())),
            ]),
        )?);
    }
    capsules.push(seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "Creature",
            kind: "core.creature",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Global,
            migration_time_micros,
        },
        legacy_id,
        vec![CapsuleRelationship {
            name: "owner".to_owned(),
            target_kind: CapsuleKind("core.user".to_owned()),
            target_id: CapsuleId(owner_user_id),
        }],
        BTreeMap::from([
            ("username".to_owned(), CapsuleValue::Text(username)),
            (
                "creature_type".to_owned(),
                CapsuleValue::Text(creature_type),
            ),
            ("public_key".to_owned(), CapsuleValue::Bytes(public_key)),
            ("status".to_owned(), CapsuleValue::Text("active".to_owned())),
            ("chain_id".to_owned(), CapsuleValue::Text(chain_id)),
            ("subchain_id".to_owned(), CapsuleValue::Text(subchain_id)),
        ]),
    )?);
    let mut wallet_source_id = Vec::with_capacity(legacy_id.len() + finance.currency.len() + 1);
    wallet_source_id.extend_from_slice(legacy_id.as_bytes());
    wallet_source_id.push(0);
    wallet_source_id.extend_from_slice(finance.currency.as_bytes());
    let wallet_id = deterministic_legacy_capsule_id("Wallet", &wallet_source_id);
    capsules.push(
        CapsuleEnvelope {
            encoding_version: 1,
            id: CapsuleId(wallet_id),
            kind: CapsuleKind("finance.wallet".to_owned()),
            storage_class: StorageClass::Finance,
            owner_scope: OwnerScope::Creature(creature_id),
            schema_version: 1,
            revision: 1,
            created_at_micros: migration_time_micros,
            updated_at_micros: migration_time_micros,
            previous_integrity: None,
            integrity_hash: CapsuleDigest {
                algorithm: "sha2-256".to_owned(),
                bytes: vec![0; 32],
            },
            tombstone: false,
            relationships: vec![CapsuleRelationship {
                name: "creature".to_owned(),
                target_kind: CapsuleKind("core.creature".to_owned()),
                target_id: CapsuleId(creature_id),
            }],
            body: Some(CapsuleValue::Object(BTreeMap::from([
                (
                    "currency".to_owned(),
                    CapsuleValue::Text(finance.currency.clone()),
                ),
                ("balance_minor".to_owned(), CapsuleValue::Integer(balance)),
                (
                    "scale".to_owned(),
                    CapsuleValue::Integer(i64::from(finance.scale)),
                ),
                ("state".to_owned(), CapsuleValue::Text("active".to_owned())),
            ]))),
        }
        .seal()
        .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?,
    );
    Ok(capsules)
}

struct LegacyCapsuleSpec<'a> {
    family: &'a str,
    kind: &'a str,
    storage_class: StorageClass,
    owner_scope: OwnerScope,
    migration_time_micros: i64,
}

fn seal_legacy_capsule(
    spec: LegacyCapsuleSpec<'_>,
    legacy_id: &str,
    relationships: Vec<CapsuleRelationship>,
    body: BTreeMap<String, CapsuleValue>,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    if legacy_id.is_empty() || spec.migration_time_micros <= 0 {
        return Err(LegacyMigrationError::Invalid(format!(
            "legacy {} has an empty ID or invalid migration time",
            spec.family
        )));
    }
    CapsuleEnvelope {
        encoding_version: 1,
        id: CapsuleId(deterministic_legacy_capsule_id(
            spec.family,
            legacy_id.as_bytes(),
        )),
        kind: CapsuleKind(spec.kind.to_owned()),
        storage_class: spec.storage_class,
        owner_scope: spec.owner_scope,
        schema_version: 1,
        revision: 1,
        created_at_micros: spec.migration_time_micros,
        updated_at_micros: spec.migration_time_micros,
        previous_integrity: None,
        integrity_hash: CapsuleDigest {
            algorithm: "sha2-256".to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: false,
        relationships,
        body: Some(CapsuleValue::Object(body)),
    }
    .seal()
    .map_err(|error| LegacyMigrationError::Contract(error.to_string()))
}

fn validate_columns(
    family: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    allowed: &[&str],
) -> LegacyMigrationResult<()> {
    if let Some(column) = columns
        .keys()
        .find(|column| !allowed.contains(&column.as_str()))
    {
        return Err(LegacyMigrationError::Invalid(format!(
            "legacy {family} contains unreviewed column {column}"
        )));
    }
    Ok(())
}

fn required_utf8_column(
    family: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    name: &str,
) -> LegacyMigrationResult<String> {
    let value = optional_utf8_column(family, columns, name)?;
    if value.is_empty() {
        return Err(LegacyMigrationError::Invalid(format!(
            "legacy {family} omits or has empty {name}"
        )));
    }
    Ok(value)
}

fn optional_utf8_column(
    family: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    name: &str,
) -> LegacyMigrationResult<String> {
    columns
        .get(name)
        .map(|value| {
            String::from_utf8(value.clone()).map_err(|_| {
                LegacyMigrationError::Invalid(format!("legacy {family} column {name} is not UTF-8"))
            })
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn required_bool_column(
    family: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    name: &str,
) -> LegacyMigrationResult<bool> {
    match columns.get(name).map(Vec::as_slice) {
        Some([0]) => Ok(false),
        Some([1]) => Ok(true),
        _ => Err(LegacyMigrationError::Invalid(format!(
            "legacy {family} column {name} is not a canonical boolean"
        ))),
    }
}

fn required_i32_le_column(
    family: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    name: &str,
) -> LegacyMigrationResult<i32> {
    let bytes: [u8; 4] = columns
        .get(name)
        .and_then(|value| value.as_slice().try_into().ok())
        .ok_or_else(|| {
            LegacyMigrationError::Invalid(format!(
                "legacy {family} column {name} is not a little-endian i32"
            ))
        })?;
    Ok(i32::from_le_bytes(bytes))
}

fn required_i64_le_column(
    family: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    name: &str,
) -> LegacyMigrationResult<i64> {
    let bytes: [u8; 8] = columns
        .get(name)
        .and_then(|value| value.as_slice().try_into().ok())
        .ok_or_else(|| {
            LegacyMigrationError::Invalid(format!(
                "legacy {family} column {name} is not a little-endian i64"
            ))
        })?;
    Ok(i64::from_le_bytes(bytes))
}

fn required_resolved_creature(
    family: &str,
    resolved_creature_legacy_id: &str,
) -> LegacyMigrationResult<[u8; 16]> {
    if resolved_creature_legacy_id.is_empty() {
        return Err(LegacyMigrationError::Invalid(format!(
            "legacy {family} has no server-resolved creature owner"
        )));
    }
    Ok(deterministic_legacy_capsule_id(
        "Creature",
        resolved_creature_legacy_id.as_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_contracts::capsule::{
        CapsuleDigest, CapsuleId, CapsuleKind, CapsuleValue, OwnerScope, StorageClass,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_rsa_public_key_pem() -> String {
        use rsa::pkcs8::{EncodePublicKey, LineEnding};
        use rsa::rand_core::OsRng;
        let private = rsa::RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        rsa::RsaPublicKey::from(&private)
            .to_public_key_pem(LineEnding::LF)
            .unwrap()
    }

    struct MemorySource(Vec<LegacyPhysicalRecord>);

    impl LegacyRecordSource for MemorySource {
        fn read_snapshot(
            &self,
            max_records: usize,
            max_bytes: usize,
        ) -> LegacyMigrationResult<LegacySnapshot> {
            let bytes = self
                .0
                .iter()
                .map(|record| record.key.len() + record.value.len())
                .sum::<usize>();
            if self.0.len() > max_records || bytes > max_bytes {
                return Err(LegacyMigrationError::Invalid(
                    "fixture exceeds bounds".to_owned(),
                ));
            }
            Ok(LegacySnapshot {
                snapshot_id: "fixture".to_owned(),
                records: self.0.clone(),
            })
        }
    }

    struct FixtureTransformer;

    impl LegacyTransformer for FixtureTransformer {
        fn manifest_digest(&self) -> [u8; 32] {
            [7; 32]
        }

        fn transform(
            &mut self,
            record: LegacyPhysicalRecord,
        ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
            if record.key != b"known" {
                return Err(LegacyMigrationError::Unmapped {
                    family: record.family,
                    key: String::from_utf8_lossy(&record.key).into_owned(),
                });
            }
            let mut capsule = CapsuleEnvelope {
                encoding_version: 1,
                id: CapsuleId(deterministic_legacy_capsule_id("fixture", &record.value)),
                kind: CapsuleKind("core.user".to_owned()),
                storage_class: StorageClass::Core,
                owner_scope: OwnerScope::Global,
                schema_version: 1,
                revision: 1,
                created_at_micros: 1,
                updated_at_micros: 1,
                previous_integrity: None,
                integrity_hash: CapsuleDigest {
                    algorithm: "sha2-256".to_owned(),
                    bytes: vec![0; 32],
                },
                tombstone: false,
                relationships: Vec::new(),
                body: Some(CapsuleValue::Object(BTreeMap::from([(
                    "username".to_owned(),
                    CapsuleValue::Text(String::from_utf8_lossy(&record.value).into_owned()),
                )]))),
            };
            capsule = capsule
                .seal()
                .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?;
            Ok(vec![capsule])
        }

        fn finish(&mut self) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
            Ok(Vec::new())
        }
    }

    #[derive(Default)]
    struct MemorySink(BTreeSet<Vec<u8>>);

    impl CapsuleImportSink for MemorySink {
        fn import(&mut self, capsule: CapsuleEnvelope) -> LegacyMigrationResult<ImportDisposition> {
            let bytes = capsule
                .canonical_bytes()
                .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?;
            Ok(if self.0.insert(bytes) {
                ImportDisposition::Inserted
            } else {
                ImportDisposition::AlreadyPresent
            })
        }
    }

    #[test]
    fn bounded_export_import_is_deterministic_resumable_and_idempotent() {
        let source = MemorySource(vec![LegacyPhysicalRecord {
            family: "fixture".to_owned(),
            key: b"known".to_vec(),
            value: b"alice".to_vec(),
        }]);
        let export = export_canonical(
            "legacy-rocksdb-v1",
            10,
            &source,
            &mut FixtureTransformer,
            10,
            1_024,
        )
        .unwrap();
        let mut sink = MemorySink::default();
        let first = import_canonical(&export, None, &mut sink, 1).unwrap();
        assert_eq!(first.inserted, 1);
        assert_eq!(first.checkpoint.next_sequence, 1);
        let complete = import_canonical(&export, Some(&first.checkpoint), &mut sink, 1).unwrap();
        assert_eq!(complete.inserted, 0);
        let replay = import_canonical(&export, None, &mut sink, 1).unwrap();
        assert_eq!(replay.already_present, 1);
    }

    #[test]
    fn unmapped_records_and_forged_checkpoints_fail_closed() {
        let source = MemorySource(vec![LegacyPhysicalRecord {
            family: "fixture".to_owned(),
            key: b"unknown".to_vec(),
            value: Vec::new(),
        }]);
        assert!(matches!(
            export_canonical(
                "legacy-rocksdb-v1",
                10,
                &source,
                &mut FixtureTransformer,
                10,
                1_024
            ),
            Err(LegacyMigrationError::Unmapped { .. })
        ));

        let good = MemorySource(vec![LegacyPhysicalRecord {
            family: "fixture".to_owned(),
            key: b"known".to_vec(),
            value: b"alice".to_vec(),
        }]);
        let export = export_canonical(
            "legacy-rocksdb-v1",
            10,
            &good,
            &mut FixtureTransformer,
            10,
            1_024,
        )
        .unwrap();
        let mut checkpoint = export.checkpoint(0).unwrap();
        checkpoint.stream_digest[0] ^= 1;
        assert!(
            import_canonical(&export, Some(&checkpoint), &mut MemorySink::default(), 1).is_err()
        );
    }

    #[test]
    fn legacy_program_fixture_maps_identity_owner_relationship_and_fields() {
        let columns = BTreeMap::from([
            ("|".to_owned(), vec![1]),
            ("id".to_owned(), b"program-one".to_vec()),
            ("machineId".to_owned(), b"creature-one".to_vec()),
            ("runtime".to_owned(), b"wasm".to_vec()),
            ("path".to_owned(), b"/programs/one".to_vec()),
            ("comment".to_owned(), b"fixture".to_vec()),
        ]);
        let capsule = transform_legacy_program("program-one", &columns, 50).unwrap();
        capsule.verify().unwrap();
        assert_eq!(capsule.kind.0, "core.program");
        let creature_id = deterministic_legacy_capsule_id("Creature", b"creature-one");
        assert_eq!(capsule.owner_scope, OwnerScope::Creature(creature_id));
        assert_eq!(capsule.relationships[0].target_id.0, creature_id);
        assert!(matches!(
            &capsule.body,
            Some(CapsuleValue::Object(body))
                if body["runtime"] == CapsuleValue::Text("wasm".to_owned())
        ));

        let mut unknown = columns;
        unknown.insert("unreviewed".to_owned(), Vec::new());
        assert!(transform_legacy_program("program-one", &unknown, 50).is_err());
    }

    #[test]
    fn legacy_build_log_requires_resolved_owner_and_converts_millis() {
        let row = LegacyBuildLogRow {
            id: "log-one".to_owned(),
            build_id: String::new(),
            machine_id: String::new(),
            vm_id: "vm-one".to_owned(),
            log_type: "runtime".to_owned(),
            data: "ready".to_owned(),
            time_millis: 1_700_000_000_123,
        };
        let capsule = transform_legacy_build_log(&row, "creature-one").unwrap();
        capsule.verify().unwrap();
        assert_eq!(capsule.kind.0, "telemetry.build_log");
        assert_eq!(capsule.storage_class, StorageClass::Telemetry);
        assert_eq!(capsule.created_at_micros, 1_700_000_000_123_000);
        assert_eq!(
            capsule.owner_scope,
            OwnerScope::Creature(deterministic_legacy_capsule_id("Creature", b"creature-one"))
        );
        assert!(matches!(
            &capsule.body,
            Some(CapsuleValue::Object(body))
                if body["workload_id"] == CapsuleValue::Text("vm-one".to_owned())
                    && body["machine_id"] == CapsuleValue::Text("creature-one".to_owned())
                    && body["observed_at_micros"]
                        == CapsuleValue::Integer(1_700_000_000_123_000)
        ));

        let mut conflicting = row.clone();
        conflicting.machine_id = "other-creature".to_owned();
        assert!(transform_legacy_build_log(&conflicting, "creature-one").is_err());
        assert!(transform_legacy_build_log(&row, "").is_err());
    }

    #[test]
    fn legacy_signal_history_gets_deterministic_per_store_sequences() {
        let policies = BTreeMap::from([
            (
                "store-a".to_owned(),
                LegacySignalStreamPolicy {
                    authorization_scope: b"scope-a".to_vec(),
                    retention_class: "permanent".to_owned(),
                },
            ),
            (
                "store-b".to_owned(),
                LegacySignalStreamPolicy {
                    authorization_scope: b"scope-b".to_vec(),
                    retention_class: "bounded".to_owned(),
                },
            ),
        ]);
        let rows = vec![
            LegacySignalRow {
                id: "later".to_owned(),
                store_id: "store-a".to_owned(),
                user_id: "user-a".to_owned(),
                data: "two".to_owned(),
                encoded_tags: "|kind=message|thread=main|".to_owned(),
                time_millis: 20,
                edited: true,
            },
            LegacySignalRow {
                id: "other".to_owned(),
                store_id: "store-b".to_owned(),
                user_id: "user-b".to_owned(),
                data: "other".to_owned(),
                encoded_tags: String::new(),
                time_millis: 30,
                edited: false,
            },
            LegacySignalRow {
                id: "earlier".to_owned(),
                store_id: "store-a".to_owned(),
                user_id: "user-a".to_owned(),
                data: "one".to_owned(),
                encoded_tags: "|kind=message|".to_owned(),
                time_millis: 10,
                edited: false,
            },
        ];
        let capsules = transform_legacy_signal_rows(rows, &policies).unwrap();
        assert_eq!(capsules.len(), 3);
        assert!(capsules.iter().all(|capsule| capsule.verify().is_ok()));
        let sequences = capsules
            .iter()
            .map(|capsule| match &capsule.body {
                Some(CapsuleValue::Object(body)) => (
                    body["stream_id"].clone(),
                    body["sequence"].clone(),
                    body["occurred_at_micros"].clone(),
                ),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            sequences,
            vec![
                (
                    CapsuleValue::Text("store:store-a".to_owned()),
                    CapsuleValue::Integer(1),
                    CapsuleValue::Integer(10_000),
                ),
                (
                    CapsuleValue::Text("store:store-a".to_owned()),
                    CapsuleValue::Integer(2),
                    CapsuleValue::Integer(20_000),
                ),
                (
                    CapsuleValue::Text("store:store-b".to_owned()),
                    CapsuleValue::Integer(1),
                    CapsuleValue::Integer(30_000),
                ),
            ]
        );
        assert!(
            transform_legacy_signal_rows(
                vec![LegacySignalRow {
                    id: "bad".to_owned(),
                    store_id: "store-a".to_owned(),
                    user_id: "user-a".to_owned(),
                    data: String::new(),
                    encoded_tags: "not-framed".to_owned(),
                    time_millis: 1,
                    edited: false,
                }],
                &policies,
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_entity_maps_composite_identity_program_and_resolved_owner() {
        let columns = BTreeMap::from([
            ("|".to_owned(), vec![1]),
            ("programId".to_owned(), b"program-one".to_vec()),
            ("entityId".to_owned(), b"worker".to_vec()),
            ("entityType".to_owned(), b"container".to_vec()),
            ("imageName".to_owned(), b"worker:v1".to_vec()),
        ]);
        let capsule =
            transform_legacy_entity("program-one::worker", &columns, "creature-one", 60).unwrap();
        capsule.verify().unwrap();
        assert_eq!(capsule.kind.0, "core.entity");
        assert_eq!(
            capsule.relationships[0].target_id.0,
            deterministic_legacy_capsule_id("Program", b"program-one")
        );
        assert!(transform_legacy_entity("wrong", &columns, "creature-one", 60).is_err());
        assert!(transform_legacy_entity("program-one::worker", &columns, "", 60).is_err());
    }

    #[test]
    fn legacy_store_decodes_binary_fields_and_resolved_creator_link() {
        let columns = BTreeMap::from([
            ("|".to_owned(), vec![1]),
            ("tag".to_owned(), b"orders".to_vec()),
            ("parentId".to_owned(), b"root-store".to_vec()),
            ("isPublic".to_owned(), vec![1]),
            ("persHist".to_owned(), vec![0]),
            ("memberCount".to_owned(), 3_i32.to_le_bytes().to_vec()),
            ("signalCount".to_owned(), 9_i64.to_le_bytes().to_vec()),
        ]);
        let capsule = transform_legacy_store("child-store", &columns, "creature-one", 70).unwrap();
        capsule.verify().unwrap();
        assert_eq!(capsule.kind.0, "core.store");
        assert_eq!(capsule.relationships.len(), 2);
        assert!(matches!(
            &capsule.body,
            Some(CapsuleValue::Object(body))
                if body["is_public"] == CapsuleValue::Bool(true)
                    && body["member_count"] == CapsuleValue::Integer(3)
                    && body["signal_count"] == CapsuleValue::Integer(9)
        ));

        let mut malformed = columns.clone();
        malformed.insert("isPublic".to_owned(), vec![2]);
        assert!(transform_legacy_store("child-store", &malformed, "creature-one", 70).is_err());
        assert!(transform_legacy_store("child-store", &columns, "", 70).is_err());
    }

    #[test]
    fn legacy_chain_and_named_shard_preserve_graph_and_owner() {
        let chain_columns = BTreeMap::from([
            ("|".to_owned(), vec![1]),
            ("id".to_owned(), b"work-chain".to_vec()),
            ("storeId".to_owned(), b"store-one".to_vec()),
        ]);
        let chain =
            transform_legacy_chain("work-chain", &chain_columns, "creature-one", 80).unwrap();
        chain.verify().unwrap();
        assert_eq!(chain.kind.0, "core.chain");
        assert!(matches!(
            &chain.body,
            Some(CapsuleValue::Object(body))
                if body["status"] == CapsuleValue::Text("active".to_owned())
        ));

        let shard_columns = BTreeMap::from([
            ("|".to_owned(), vec![1]),
            ("id".to_owned(), b"shard-main".to_vec()),
            ("workChainId".to_owned(), b"work-chain".to_vec()),
        ]);
        let shard =
            transform_legacy_chain_shard("shard-main", &shard_columns, "creature-one", 80).unwrap();
        shard.verify().unwrap();
        assert_eq!(shard.kind.0, "core.chain_shard");
        assert_eq!(shard.relationships[0].target_id.0, chain.id.0);
        assert!(matches!(
            &shard.body,
            Some(CapsuleValue::Object(body))
                if body["shard_name"] == CapsuleValue::Text("shard-main".to_owned())
        ));
    }

    #[test]
    fn snapshot_graph_resolves_owners_independent_of_physical_order() {
        let mut records = Vec::new();
        let mut add_object = |family: &str, id: &str, columns: BTreeMap<&str, Vec<u8>>| {
            for (column, value) in columns {
                records.push(LegacyPhysicalRecord {
                    family: "application-rocksdb-default".to_owned(),
                    key: format!("obj::{family}::{id}::{column}").into_bytes(),
                    value,
                });
            }
        };
        add_object(
            "Program",
            "program-one",
            BTreeMap::from([
                ("|", vec![1]),
                ("id", b"program-one".to_vec()),
                ("machineId", b"creature-one".to_vec()),
                ("runtime", b"wasm".to_vec()),
                ("path", b"/one".to_vec()),
                ("comment", Vec::new()),
            ]),
        );
        add_object(
            "Entity",
            "program-one::worker",
            BTreeMap::from([
                ("|", vec![1]),
                ("programId", b"program-one".to_vec()),
                ("entityId", b"worker".to_vec()),
                ("entityType", b"wasm".to_vec()),
                ("imageName", b"worker".to_vec()),
            ]),
        );
        add_object(
            "Store",
            "store-one",
            BTreeMap::from([
                ("|", vec![1]),
                ("tag", b"events".to_vec()),
                ("parentId", Vec::new()),
                ("isPublic", vec![0]),
                ("persHist", vec![1]),
                ("memberCount", 1_i32.to_le_bytes().to_vec()),
                ("signalCount", 2_i64.to_le_bytes().to_vec()),
            ]),
        );
        add_object(
            "Chain",
            "chain-one",
            BTreeMap::from([
                ("|", vec![1]),
                ("id", b"chain-one".to_vec()),
                ("storeId", b"store-one".to_vec()),
            ]),
        );
        add_object(
            "ChainShard",
            "shard-main",
            BTreeMap::from([
                ("|", vec![1]),
                ("id", b"shard-main".to_vec()),
                ("workChainId", b"chain-one".to_vec()),
            ]),
        );
        records.push(LegacyPhysicalRecord {
            family: "application-rocksdb-default".to_owned(),
            key: b"link::creatorof::creature-one::store-one".to_vec(),
            value: b"true".to_vec(),
        });
        records.reverse();

        let graph = LegacySnapshotGraph::assemble(records).unwrap();
        let capsules = graph.transform_reviewed(90).unwrap();
        assert_eq!(capsules.len(), 5);
        assert!(capsules.windows(2).all(|pair| {
            (pair[0].kind.0.as_str(), pair[0].id.0) <= (pair[1].kind.0.as_str(), pair[1].id.0)
        }));
        assert!(capsules.iter().all(|capsule| capsule.verify().is_ok()));
    }

    #[test]
    fn snapshot_graph_rejects_raw_keys_and_unreviewed_typed_families() {
        assert!(matches!(
            LegacySnapshotGraph::assemble(vec![LegacyPhysicalRecord {
                family: "application-rocksdb-default".to_owned(),
                key: b"globalIdCounter".to_vec(),
                value: vec![1],
            }]),
            Err(LegacyMigrationError::Unmapped { .. })
        ));
        let graph = LegacySnapshotGraph::assemble(vec![LegacyPhysicalRecord {
            family: "application-rocksdb-default".to_owned(),
            key: b"obj::File::file-one::ownerId".to_vec(),
            value: b"user-one".to_vec(),
        }])
        .unwrap();
        assert!(matches!(
            graph.transform_reviewed(100),
            Err(LegacyMigrationError::Unmapped { family, .. }) if family == "File.artifact"
        ));
    }

    #[test]
    fn legacy_session_becomes_deterministic_revocation_not_live_credential() {
        let columns = BTreeMap::from([
            ("|".to_owned(), vec![1]),
            ("userId".to_owned(), b"user-one".to_vec()),
        ]);
        let capsule =
            transform_legacy_session_revocation("secret-session", &columns, "user-one", 110)
                .unwrap();
        capsule.verify().unwrap();
        assert_eq!(capsule.kind.0, "core.session");
        assert!(matches!(
            &capsule.body,
            Some(CapsuleValue::Object(body))
                if body["issued_at_micros"] == CapsuleValue::Integer(0)
                    && body["expires_at_micros"] == CapsuleValue::Integer(0)
                    && body["revoked_at_micros"] == CapsuleValue::Integer(110)
                    && matches!(&body["token_digest"], CapsuleValue::Bytes(bytes) if bytes.len() == 32)
        ));
        assert_ne!(
            match capsule.body.unwrap() {
                CapsuleValue::Object(body) => body["token_digest"].clone(),
                _ => unreachable!(),
            },
            CapsuleValue::Bytes(b"secret-session".to_vec())
        );
    }

    #[test]
    fn legacy_file_requires_digest_evidence_for_external_bytes() {
        let columns = BTreeMap::from([
            ("|".to_owned(), vec![1]),
            ("storeId".to_owned(), b"store-one".to_vec()),
            ("ownerId".to_owned(), b"user-one".to_vec()),
        ]);
        let evidence = LegacyFileArtifactEvidence::from_bytes(
            "files/store-one/file-one",
            "text/plain",
            b"hello",
        )
        .unwrap();
        let capsule =
            transform_legacy_file("file-one", &columns, &evidence, "user-one", 120).unwrap();
        capsule.verify().unwrap();
        assert_eq!(capsule.kind.0, "core.file");
        assert_eq!(capsule.relationships.len(), 2);
        assert!(matches!(
            &capsule.body,
            Some(CapsuleValue::Object(body))
                if body["size_bytes"] == CapsuleValue::Integer(5)
                    && matches!(&body["content_digest"], CapsuleValue::Bytes(bytes) if bytes.len() == 32)
        ));
    }

    #[test]
    fn legacy_creature_splits_human_boundary_and_configured_wallet() {
        let public_key = test_rsa_public_key_pem();
        let columns = BTreeMap::from([
            ("|".to_owned(), vec![1]),
            ("type".to_owned(), b"human".to_vec()),
            ("username".to_owned(), b"alice@example".to_vec()),
            ("publicKey".to_owned(), public_key.as_bytes().to_vec()),
            ("chainId".to_owned(), b"main".to_vec()),
            ("subchainId".to_owned(), b"shard-main".to_vec()),
            ("ownerId".to_owned(), b"free".to_vec()),
            ("balance".to_owned(), 125_i64.to_le_bytes().to_vec()),
        ]);
        let capsules = transform_legacy_creature(
            "human-one",
            &columns,
            "human-one",
            Some("alice@example.test"),
            &LegacyFinanceConfig {
                currency: "ASE".to_owned(),
                scale: 2,
            },
            130,
        )
        .unwrap();
        assert_eq!(capsules.len(), 3);
        assert_eq!(capsules[0].kind.0, "core.user");
        assert_eq!(capsules[1].kind.0, "core.creature");
        assert_eq!(capsules[2].kind.0, "finance.wallet");
        assert!(capsules.iter().all(|capsule| capsule.verify().is_ok()));
        assert!(matches!(
            &capsules[1].body,
            Some(CapsuleValue::Object(body))
                if body["chain_id"] == CapsuleValue::Text("main".to_owned())
                    && matches!(&body["public_key"], CapsuleValue::Bytes(bytes) if bytes.starts_with(&[0x85, 0x24]))
        ));
        assert!(matches!(
            &capsules[2].body,
            Some(CapsuleValue::Object(body))
                if body["balance_minor"] == CapsuleValue::Integer(125)
                    && body["currency"] == CapsuleValue::Text("ASE".to_owned())
                    && body["scale"] == CapsuleValue::Integer(2)
        ));
        assert!(
            transform_legacy_creature(
                "human-one",
                &columns,
                "other-user",
                None,
                &LegacyFinanceConfig {
                    currency: "ASE".to_owned(),
                    scale: 2,
                },
                130,
            )
            .is_err()
        );
    }

    #[test]
    fn rocksdb_source_is_read_only_and_enforces_snapshot_bounds() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aseman-storage-legacy-test-{}-{unique}",
            std::process::id()
        ));
        assert!(!path.exists());
        {
            let database = DB::open_default(&path).unwrap();
            database.put(b"known", b"alice").unwrap();
        }
        let source = RocksDbLegacySource::open_read_only(&path, "snapshot-one").unwrap();
        let snapshot = source.read_snapshot(1, 64).unwrap();
        assert_eq!(snapshot.snapshot_id, "snapshot-one");
        assert_eq!(snapshot.records.len(), 1);
        assert!(source.read_snapshot(1, 4).is_err());
        drop(source);
        DB::destroy(&Options::default(), &path).unwrap();
        assert!(!path.exists());
    }
}
