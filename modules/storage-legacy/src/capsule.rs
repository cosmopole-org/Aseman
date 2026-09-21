//! Deterministic legacy identity, capsule sealing, and strict column decoding.

use super::*;

pub use aseman_contracts::legacy_realtime::deterministic_legacy_capsule_id;

pub(crate) struct LegacyCapsuleSpec<'a> {
    pub(crate) family: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) storage_class: StorageClass,
    pub(crate) owner_scope: OwnerScope,
    pub(crate) migration_time_micros: i64,
}

pub(crate) fn seal_legacy_capsule(
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

pub(crate) fn validate_columns(
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

pub(crate) fn required_utf8_column(
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

pub(crate) fn optional_utf8_column(
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

pub(crate) fn required_bool_column(
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

pub(crate) fn required_i32_le_column(
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

pub(crate) fn required_i64_le_column(
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

pub(crate) fn required_resolved_creature(
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
