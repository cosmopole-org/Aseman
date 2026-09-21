//! Provider-neutral, resumable canonical capsule export/import framing.
#![forbid(unsafe_code)]

use crate::capsule::{CapsuleEnvelope, CapsuleError};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

pub const CAPSULE_EXPORT_FORMAT_VERSION: u16 = 1;
pub const MAX_EXPORT_PROVIDER_ID_BYTES: usize = 128;
pub const MAX_EXPORT_SNAPSHOT_ID_BYTES: usize = 256;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum MigrationContractError {
    #[error("invalid migration stream: {0}")]
    Invalid(String),
    #[error("invalid capsule in migration stream: {0}")]
    Capsule(String),
}

impl From<CapsuleError> for MigrationContractError {
    fn from(error: CapsuleError) -> Self {
        Self::Capsule(error.to_string())
    }
}

pub type MigrationContractResult<T> = Result<T, MigrationContractError>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleExportHeader {
    pub format_version: u16,
    pub source_provider: String,
    pub source_snapshot_id: String,
    pub transform_manifest_digest: Vec<u8>,
    pub created_at_micros: i64,
}

impl CapsuleExportHeader {
    pub fn validate(&self) -> MigrationContractResult<()> {
        if self.format_version != CAPSULE_EXPORT_FORMAT_VERSION {
            return Err(invalid("unsupported capsule export format"));
        }
        if self.source_provider.is_empty()
            || self.source_provider.len() > MAX_EXPORT_PROVIDER_ID_BYTES
            || self.source_snapshot_id.is_empty()
            || self.source_snapshot_id.len() > MAX_EXPORT_SNAPSHOT_ID_BYTES
            || self.transform_manifest_digest.len() != 32
        {
            return Err(invalid("export header identifiers or digest are invalid"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleExportRecord {
    pub sequence: u64,
    pub capsule_cbor: Vec<u8>,
}

impl CapsuleExportRecord {
    pub fn decode(&self) -> MigrationContractResult<CapsuleEnvelope> {
        Ok(CapsuleEnvelope::from_canonical_bytes(&self.capsule_cbor)?)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleExportCheckpoint {
    pub next_sequence: u64,
    pub record_count: u64,
    pub stream_digest: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleExportTrailer {
    pub record_count: u64,
    pub stream_digest: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalCapsuleExport {
    pub header: CapsuleExportHeader,
    pub records: Vec<CapsuleExportRecord>,
    pub trailer: CapsuleExportTrailer,
}

impl CanonicalCapsuleExport {
    pub fn build(
        header: CapsuleExportHeader,
        capsules: impl IntoIterator<Item = CapsuleEnvelope>,
    ) -> MigrationContractResult<Self> {
        header.validate()?;
        let records = capsules
            .into_iter()
            .enumerate()
            .map(|(sequence, capsule)| {
                let sequence = u64::try_from(sequence)
                    .map_err(|_| invalid("capsule export sequence overflow"))?;
                Ok(CapsuleExportRecord {
                    sequence,
                    capsule_cbor: capsule.canonical_bytes()?,
                })
            })
            .collect::<MigrationContractResult<Vec<_>>>()?;
        let record_count = u64::try_from(records.len())
            .map_err(|_| invalid("capsule export record count overflow"))?;
        let stream_digest = stream_digest(&header, &records)?;
        Ok(Self {
            header,
            records,
            trailer: CapsuleExportTrailer {
                record_count,
                stream_digest,
            },
        })
    }

    pub fn validate(&self) -> MigrationContractResult<()> {
        self.header.validate()?;
        let expected_count = u64::try_from(self.records.len())
            .map_err(|_| invalid("capsule export record count overflow"))?;
        if self.trailer.record_count != expected_count || self.trailer.stream_digest.len() != 32 {
            return Err(invalid("export trailer count or digest is invalid"));
        }
        for (expected, record) in self.records.iter().enumerate() {
            let expected =
                u64::try_from(expected).map_err(|_| invalid("capsule export sequence overflow"))?;
            if record.sequence != expected {
                return Err(invalid("export records are not contiguous and ordered"));
            }
            record.decode()?;
        }
        if stream_digest(&self.header, &self.records)? != self.trailer.stream_digest {
            return Err(invalid("export stream digest mismatch"));
        }
        Ok(())
    }

    pub fn checkpoint(
        &self,
        next_sequence: u64,
    ) -> MigrationContractResult<CapsuleExportCheckpoint> {
        self.validate()?;
        if next_sequence > self.trailer.record_count {
            return Err(invalid("checkpoint sequence exceeds the export"));
        }
        let count =
            usize::try_from(next_sequence).map_err(|_| invalid("checkpoint sequence overflow"))?;
        Ok(CapsuleExportCheckpoint {
            next_sequence,
            record_count: next_sequence,
            stream_digest: stream_digest(&self.header, &self.records[..count])?,
        })
    }
}

fn stream_digest(
    header: &CapsuleExportHeader,
    records: &[CapsuleExportRecord],
) -> MigrationContractResult<Vec<u8>> {
    let mut hasher = Sha256::new();
    hasher.update(b"ASEMAN-CAPSULE-EXPORT-V1\0");
    digest_field(&mut hasher, header.source_provider.as_bytes())?;
    digest_field(&mut hasher, header.source_snapshot_id.as_bytes())?;
    digest_field(&mut hasher, &header.transform_manifest_digest)?;
    hasher.update(header.created_at_micros.to_be_bytes());
    for record in records {
        hasher.update(record.sequence.to_be_bytes());
        digest_field(&mut hasher, &record.capsule_cbor)?;
    }
    Ok(hasher.finalize().to_vec())
}

fn digest_field(hasher: &mut Sha256, value: &[u8]) -> MigrationContractResult<()> {
    let length = u64::try_from(value.len()).map_err(|_| invalid("digest field is too large"))?;
    hasher.update(length.to_be_bytes());
    hasher.update(value);
    Ok(())
}

fn invalid(message: &str) -> MigrationContractError {
    MigrationContractError::Invalid(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capsule::{
        CapsuleDigest, CapsuleId, CapsuleKind, CapsuleValue, OwnerScope, StorageClass,
    };
    use std::collections::BTreeMap;

    fn capsule(id: u8) -> CapsuleEnvelope {
        CapsuleEnvelope {
            encoding_version: 1,
            id: CapsuleId([id; 16]),
            kind: CapsuleKind("core.user".to_owned()),
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Global,
            schema_version: 1,
            revision: 1,
            created_at_micros: 10,
            updated_at_micros: 10,
            previous_integrity: None,
            integrity_hash: CapsuleDigest {
                algorithm: "sha2-256".to_owned(),
                bytes: vec![0; 32],
            },
            tombstone: false,
            relationships: Vec::new(),
            body: Some(CapsuleValue::Object(BTreeMap::from([(
                "username".to_owned(),
                CapsuleValue::Text(format!("user-{id}")),
            )]))),
        }
        .seal()
        .unwrap()
    }

    fn header() -> CapsuleExportHeader {
        CapsuleExportHeader {
            format_version: 1,
            source_provider: "legacy-rocksdb-v1".to_owned(),
            source_snapshot_id: "fixture-snapshot".to_owned(),
            transform_manifest_digest: vec![9; 32],
            created_at_micros: 20,
        }
    }

    #[test]
    fn canonical_export_detects_tampering_and_supports_resume_checkpoints() {
        let export = CanonicalCapsuleExport::build(header(), [capsule(1), capsule(2)]).unwrap();
        export.validate().unwrap();
        let checkpoint = export.checkpoint(1).unwrap();
        assert_eq!(checkpoint.next_sequence, 1);
        assert_ne!(checkpoint.stream_digest, export.trailer.stream_digest);

        let mut tampered = export.clone();
        tampered.records[0].capsule_cbor[5] ^= 1;
        assert!(tampered.validate().is_err());
        let mut reordered = export;
        reordered.records.swap(0, 1);
        assert!(reordered.validate().is_err());
    }
}

/// A309 semantic digest: what a record means, independent of the revision chain.
///
/// It covers kind, identity, storage class, owner, tombstone, relationships, and body.
/// It excludes revision, timestamps, and integrity links, so a record re-applied as a
/// new revision still compares equal to its source (ADR 0005: read-compare uses domain
/// semantics, not physical bytes).
pub fn semantic_digest(capsule: &CapsuleEnvelope) -> MigrationContractResult<[u8; 32]> {
    use crate::capsule::{CapsuleValue, encode_value};
    use std::collections::BTreeMap;
    let owner = match &capsule.owner_scope {
        crate::capsule::OwnerScope::Global => CapsuleValue::Text("global".to_owned()),
        crate::capsule::OwnerScope::Node(id) => CapsuleValue::Array(vec![
            CapsuleValue::Text("node".to_owned()),
            CapsuleValue::Bytes(id.to_vec()),
        ]),
        crate::capsule::OwnerScope::Creature(id) => CapsuleValue::Array(vec![
            CapsuleValue::Text("creature".to_owned()),
            CapsuleValue::Bytes(id.to_vec()),
        ]),
        crate::capsule::OwnerScope::Module(name) => CapsuleValue::Array(vec![
            CapsuleValue::Text("module".to_owned()),
            CapsuleValue::Text(name.clone()),
        ]),
    };
    let mut relationships = capsule
        .relationships
        .iter()
        .map(|relationship| {
            CapsuleValue::Array(vec![
                CapsuleValue::Text(relationship.name.clone()),
                CapsuleValue::Text(relationship.target_kind.0.clone()),
                CapsuleValue::Bytes(relationship.target_id.0.to_vec()),
            ])
        })
        .collect::<Vec<_>>();
    relationships.sort_by_key(|value| format!("{value:?}"));
    let semantic = CapsuleValue::Object(BTreeMap::from([
        (
            "kind".to_owned(),
            CapsuleValue::Text(capsule.kind.0.clone()),
        ),
        ("id".to_owned(), CapsuleValue::Bytes(capsule.id.0.to_vec())),
        (
            "storage_class".to_owned(),
            CapsuleValue::Text(format!("{:?}", capsule.storage_class)),
        ),
        ("owner".to_owned(), owner),
        (
            "tombstone".to_owned(),
            CapsuleValue::Bool(capsule.tombstone),
        ),
        (
            "relationships".to_owned(),
            CapsuleValue::Array(relationships),
        ),
        (
            "body".to_owned(),
            capsule.body.clone().unwrap_or(CapsuleValue::Null),
        ),
    ]));
    let encoded = encode_value(&semantic)?;
    let mut hasher = Sha256::new();
    hasher.update(b"ASEMAN-CAPSULE-SEMANTIC-DIGEST-V1\0");
    hasher.update((encoded.len() as u64).to_be_bytes());
    hasher.update(&encoded);
    Ok(hasher.finalize().into())
}

/// One target write produced by delta planning, with its optimistic revision.
#[derive(Clone, Debug, PartialEq)]
pub struct DeltaWrite {
    pub capsule: CapsuleEnvelope,
    /// `None` inserts revision 1; `Some(r)` replaces revision `r`.
    pub expected_revision: Option<u64>,
}

/// Plan the target writes that make `target` semantically equal to `source`.
///
/// Missing records are inserted as-is. A changed record becomes the next revision of
/// the target record, chained to its integrity hash with its creation time preserved.
/// A record the source no longer has becomes a tombstone revision. Output order is
/// deterministic (kind, then ID).
pub fn plan_delta(
    source: &[CapsuleEnvelope],
    target: &[CapsuleEnvelope],
    now_micros: i64,
) -> MigrationContractResult<Vec<DeltaWrite>> {
    use std::collections::BTreeMap;
    let key = |capsule: &CapsuleEnvelope| (capsule.kind.0.clone(), capsule.id.0);
    let targets = target
        .iter()
        .map(|capsule| (key(capsule), capsule))
        .collect::<BTreeMap<_, _>>();
    let sources = source
        .iter()
        .map(|capsule| (key(capsule), capsule))
        .collect::<BTreeMap<_, _>>();
    let next = |current: &CapsuleEnvelope,
                mut next: CapsuleEnvelope|
     -> MigrationContractResult<DeltaWrite> {
        next.revision = current
            .revision
            .checked_add(1)
            .ok_or_else(|| MigrationContractError::Invalid("revision overflow".to_owned()))?;
        next.created_at_micros = current.created_at_micros;
        next.updated_at_micros = now_micros.max(current.updated_at_micros);
        next.previous_integrity = Some(current.integrity_hash.clone());
        Ok(DeltaWrite {
            capsule: next.seal()?,
            expected_revision: Some(current.revision),
        })
    };
    let mut writes = Vec::new();
    for (identity, source) in &sources {
        match targets.get(identity) {
            None => writes.push(DeltaWrite {
                capsule: (*source).clone(),
                expected_revision: None,
            }),
            Some(current) if semantic_digest(current)? != semantic_digest(source)? => {
                writes.push(next(current, (*source).clone())?);
            }
            Some(_) => {}
        }
    }
    for (identity, current) in &targets {
        if !sources.contains_key(identity) && !current.tombstone {
            let tombstone = CapsuleEnvelope {
                tombstone: true,
                body: None,
                ..(*current).clone()
            };
            writes.push(next(current, tombstone)?);
        }
    }
    Ok(writes)
}

#[cfg(test)]
mod delta_tests {
    use super::*;
    use crate::capsule::{
        CapsuleDigest, CapsuleId, CapsuleKind, CapsuleValue, OwnerScope, StorageClass,
    };
    use std::collections::BTreeMap;

    fn user(id: u8, name: &str, at: i64) -> CapsuleEnvelope {
        CapsuleEnvelope {
            encoding_version: 1,
            id: CapsuleId([id; 16]),
            kind: CapsuleKind("core.user".to_owned()),
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Global,
            schema_version: 1,
            revision: 1,
            created_at_micros: at,
            updated_at_micros: at,
            previous_integrity: None,
            integrity_hash: CapsuleDigest {
                algorithm: "sha2-256".to_owned(),
                bytes: vec![0; 32],
            },
            tombstone: false,
            relationships: Vec::new(),
            body: Some(CapsuleValue::Object(BTreeMap::from([(
                "username".to_owned(),
                CapsuleValue::Text(name.to_owned()),
            )]))),
        }
        .seal()
        .unwrap()
    }

    #[test]
    fn semantic_digest_ignores_the_revision_chain_but_not_meaning() {
        let original = user(1, "alice", 10);
        let reissued = user(1, "alice", 99);
        assert_ne!(
            original.canonical_bytes().unwrap(),
            reissued.canonical_bytes().unwrap()
        );
        assert_eq!(
            semantic_digest(&original).unwrap(),
            semantic_digest(&reissued).unwrap()
        );
        assert_ne!(
            semantic_digest(&original).unwrap(),
            semantic_digest(&user(1, "bob", 10)).unwrap()
        );
    }

    #[test]
    fn delta_inserts_chains_replacements_and_tombstones_removed_records() {
        // Target state imported at t=10; the legacy source changed afterwards.
        let target = [
            user(1, "alice", 10),
            user(2, "bob", 10),
            user(3, "carol", 10),
        ];
        let source = [
            user(1, "alice", 50),
            user(2, "robert", 50),
            user(4, "dave", 50),
        ];
        let writes = plan_delta(&source, &target, 60).unwrap();
        assert_eq!(writes.len(), 3);

        let replaced = writes
            .iter()
            .find(|write| write.capsule.id.0 == [2; 16])
            .unwrap();
        assert_eq!(replaced.expected_revision, Some(1));
        assert_eq!(replaced.capsule.revision, 2);
        assert_eq!(replaced.capsule.created_at_micros, 10);
        assert_eq!(
            replaced.capsule.previous_integrity.as_ref(),
            Some(&target[1].integrity_hash)
        );
        replaced.capsule.verify().unwrap();

        let inserted = writes
            .iter()
            .find(|write| write.capsule.id.0 == [4; 16])
            .unwrap();
        assert_eq!(inserted.expected_revision, None);

        let tombstone = writes
            .iter()
            .find(|write| write.capsule.id.0 == [3; 16])
            .unwrap();
        assert!(tombstone.capsule.tombstone && tombstone.capsule.body.is_none());
        tombstone.capsule.verify().unwrap();

        // Applying the plan makes the live sets semantically equal, and a re-plan is empty.
        let mut applied: BTreeMap<[u8; 16], CapsuleEnvelope> = target
            .iter()
            .map(|capsule| (capsule.id.0, capsule.clone()))
            .collect();
        for write in &writes {
            applied.insert(write.capsule.id.0, write.capsule.clone());
        }
        let live = applied
            .values()
            .filter(|capsule| !capsule.tombstone)
            .cloned()
            .collect::<Vec<_>>();
        for capsule in &source {
            let target = live
                .iter()
                .find(|candidate| candidate.id == capsule.id)
                .unwrap();
            assert_eq!(
                semantic_digest(target).unwrap(),
                semantic_digest(capsule).unwrap()
            );
        }
        let all = applied.values().cloned().collect::<Vec<_>>();
        assert!(plan_delta(&source, &all, 70).unwrap().is_empty());
    }
}
