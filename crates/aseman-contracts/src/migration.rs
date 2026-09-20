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
