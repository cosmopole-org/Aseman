//! Read-only RocksDB snapshot source plus bounded canonical export and idempotent import.

use super::*;

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
