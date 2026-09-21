//! ADR 0016 transforms for reviewed legacy `json::` document families.

use super::*;

/// A reviewed legacy JSON document family (ADR 0016). Anything absent from this
/// table is an unreviewed `json::` key and fails closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LegacyDocumentFamily {
    pub key_prefix: &'static str,
    pub path: &'static str,
    pub capsule_family: &'static str,
    pub kind: &'static str,
    pub subject: LegacyDocumentSubject,
}

/// The legacy object family that owns a reviewed document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegacyDocumentSubject {
    /// `UserMeta::{id}` and `CreatMeta::{id}` both address a legacy `Creature` row.
    Creature {
        relationship: &'static str,
    },
    Store,
    Program,
}

pub const REVIEWED_DOCUMENT_FAMILIES: [LegacyDocumentFamily; 4] = [
    LegacyDocumentFamily {
        key_prefix: "UserMeta::",
        path: "metadata",
        capsule_family: "UserMetadata",
        kind: "core.user_metadata",
        subject: LegacyDocumentSubject::Creature {
            relationship: "creature",
        },
    },
    LegacyDocumentFamily {
        key_prefix: "CreatMeta::",
        path: "metadata",
        capsule_family: "CreatureMetadata",
        kind: "core.creature_metadata",
        subject: LegacyDocumentSubject::Creature {
            relationship: "creature",
        },
    },
    LegacyDocumentFamily {
        key_prefix: "StoreMeta::",
        path: "metadata",
        capsule_family: "StoreMetadata",
        kind: "core.store_metadata",
        subject: LegacyDocumentSubject::Store,
    },
    LegacyDocumentFamily {
        key_prefix: "ProgMeta::",
        path: "metadata",
        capsule_family: "ProgramMetadata",
        kind: "core.program_metadata",
        subject: LegacyDocumentSubject::Program,
    },
];

#[must_use]
pub fn reviewed_document_family(key: &str) -> Option<(LegacyDocumentFamily, &str)> {
    REVIEWED_DOCUMENT_FAMILIES.iter().find_map(|family| {
        key.strip_prefix(family.key_prefix)
            .filter(|legacy_id| !legacy_id.is_empty())
            .map(|legacy_id| (*family, legacy_id))
    })
}

/// Decode one legacy JSON document record into a strict capsule value.
pub(crate) fn parse_legacy_document_value(
    key: &str,
    path: &str,
    value: &[u8],
) -> LegacyMigrationResult<Value> {
    serde_json::from_slice(value).map_err(|error| {
        LegacyMigrationError::Invalid(format!(
            "legacy JSON record json::{key}::{path} is not valid JSON: {error}"
        ))
    })
}

/// Rebuild the derived `path.*` records that `index_json` splats for a document.
pub(crate) fn rebuild_derived_records(
    prefix: &str,
    object: &Map<String, Value>,
    out: &mut BTreeMap<String, Value>,
) {
    for (member, value) in object {
        if value.is_null() {
            continue;
        }
        let path = format!("{prefix}.{member}");
        if let Value::Object(child) = value {
            rebuild_derived_records(&path, child, out);
        }
        out.insert(path, value.clone());
    }
}

/// Convert decoded legacy JSON into a canonical capsule value without widening,
/// truncating, or reordering any member.
pub(crate) fn legacy_json_to_capsule_value(
    key: &str,
    value: &Value,
) -> LegacyMigrationResult<CapsuleValue> {
    Ok(match value {
        Value::Null => CapsuleValue::Null,
        Value::Bool(value) => CapsuleValue::Bool(*value),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                CapsuleValue::Integer(value)
            } else if let Some(value) = number
                .is_f64()
                .then(|| number.as_f64())
                .flatten()
                .filter(|value| value.is_finite())
            {
                // A `u64` above `i64::MAX` would round silently as a float; reject it.
                CapsuleValue::Float(value)
            } else {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy document {key} contains a number outside the capsule range"
                )));
            }
        }
        Value::String(value) => CapsuleValue::Text(value.clone()),
        Value::Array(items) => CapsuleValue::Array(
            items
                .iter()
                .map(|item| legacy_json_to_capsule_value(key, item))
                .collect::<LegacyMigrationResult<Vec<_>>>()?,
        ),
        Value::Object(members) => {
            let mut converted = BTreeMap::new();
            for (member, value) in members {
                if converted
                    .insert(member.clone(), legacy_json_to_capsule_value(key, value)?)
                    .is_some()
                {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "legacy document {key} repeats member {member}"
                    )));
                }
            }
            CapsuleValue::Object(converted)
        }
    })
}

/// Domain-separated digest over the canonical encoding of a document value.
pub(crate) fn legacy_document_digest(document: &CapsuleValue) -> LegacyMigrationResult<Vec<u8>> {
    let encoded = encode_value(document)
        .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(b"ASEMAN-LEGACY-DOCUMENT-DIGEST-V1\0");
    hasher.update((encoded.len() as u64).to_be_bytes());
    hasher.update(&encoded);
    Ok(hasher.finalize().to_vec())
}

/// Verify one legacy JSON document root against every record stored under it.
///
/// `records` holds the root path and its `root.*` splats for one legacy key. The root
/// is the only authority; every splat must equal its rebuild from the root, and a
/// stale, unequal, missing, or foreign-path splat fails closed (ADR 0016).
pub(crate) fn verified_legacy_document(
    key: &str,
    root_path: &str,
    records: &BTreeMap<String, Vec<u8>>,
) -> LegacyMigrationResult<Map<String, Value>> {
    let root = records.get(root_path).ok_or_else(|| {
        LegacyMigrationError::Invalid(format!(
            "legacy document {key} has derived records without the authoritative {root_path} record"
        ))
    })?;
    let document = match parse_legacy_document_value(key, root_path, root)? {
        Value::Object(document) => document,
        _ => {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy document json::{key}::{root_path} is not an object"
            )));
        }
    };
    let mut expected = BTreeMap::new();
    rebuild_derived_records(root_path, &document, &mut expected);
    let derived_prefix = format!("{root_path}.");
    for (path, value) in records {
        if path == root_path {
            continue;
        }
        if !path.starts_with(&derived_prefix) {
            return Err(LegacyMigrationError::Unmapped {
                family: "json-document-path".to_owned(),
                key: format!("json::{key}::{path}"),
            });
        }
        let stored = parse_legacy_document_value(key, path, value)?;
        match expected.get(path) {
            Some(rebuilt) if *rebuilt == stored => {}
            Some(_) => {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy derived record json::{key}::{path} disagrees with its document"
                )));
            }
            None => {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy derived record json::{key}::{path} is stale; the document does not contain it"
                )));
            }
        }
    }
    for path in expected.keys() {
        if !records.contains_key(path) {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy document {key} omits the derived record json::{key}::{path}"
            )));
        }
    }
    Ok(document)
}

/// Fixture-backed transform for one reviewed legacy JSON document family.
///
/// `records` holds every `json::{key}::*` record of one legacy key. The record at
/// the reviewed root path is the only authority; every other record is rebuilt and
/// compared, so stale legacy splats fail closed instead of being migrated.
pub fn transform_legacy_document(
    family: LegacyDocumentFamily,
    legacy_id: &str,
    records: &BTreeMap<String, Vec<u8>>,
    subject_relationship: CapsuleRelationship,
    owner_scope: OwnerScope,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    let key = format!("{}{legacy_id}", family.key_prefix);
    let document = verified_legacy_document(&key, family.path, records)?;
    let entry_count = i64::try_from(document.len()).map_err(|_| {
        LegacyMigrationError::Invalid(format!("legacy document {key} is too large to count"))
    })?;
    let document = legacy_json_to_capsule_value(&key, &Value::Object(document))?;
    let content_digest = legacy_document_digest(&document)?;
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: family.capsule_family,
            kind: family.kind,
            storage_class: StorageClass::Core,
            owner_scope,
            migration_time_micros,
        },
        legacy_id,
        vec![subject_relationship],
        BTreeMap::from([
            ("document".to_owned(), document),
            (
                "document_path".to_owned(),
                CapsuleValue::Text(family.path.to_owned()),
            ),
            ("entry_count".to_owned(), CapsuleValue::Integer(entry_count)),
            (
                "content_digest".to_owned(),
                CapsuleValue::Bytes(content_digest),
            ),
        ]),
    )
}

pub const LEGACY_CREATURE_TYPE_PREFIX: &str = "Json::CreatureType::";

impl LegacySnapshotGraph {
    /// Export the creature type registry (ADR 0016 amendment) and verify that each
    /// `CreatureTypeExists` flag matches a registered spec exactly.
    pub(crate) fn transform_legacy_creature_types(
        &self,
        migration_time_micros: i64,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let mut capsules = Vec::new();
        let mut registered = BTreeSet::new();
        for (key, records) in &self.documents {
            let Some(type_name) = key
                .strip_prefix(LEGACY_CREATURE_TYPE_PREFIX)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            let document = verified_legacy_document(key, "spec", records)?;
            let entry_count = i64::try_from(document.len()).map_err(|_| {
                LegacyMigrationError::Invalid(format!(
                    "legacy creature type {type_name} is too large"
                ))
            })?;
            let document = legacy_json_to_capsule_value(key, &Value::Object(document))?;
            let content_digest = legacy_document_digest(&document)?;
            capsules.push(seal_legacy_capsule(
                LegacyCapsuleSpec {
                    family: "CreatureType",
                    kind: "core.creature_type",
                    storage_class: StorageClass::Core,
                    owner_scope: OwnerScope::Global,
                    migration_time_micros,
                },
                type_name,
                Vec::new(),
                BTreeMap::from([
                    (
                        "type_name".to_owned(),
                        CapsuleValue::Text(type_name.to_owned()),
                    ),
                    ("document".to_owned(), document),
                    (
                        "document_path".to_owned(),
                        CapsuleValue::Text("spec".to_owned()),
                    ),
                    ("entry_count".to_owned(), CapsuleValue::Integer(entry_count)),
                    (
                        "content_digest".to_owned(),
                        CapsuleValue::Bytes(content_digest),
                    ),
                ]),
            )?);
            registered.insert(type_name.to_owned());
        }
        let mut flagged = BTreeSet::new();
        for (link, value) in &self.links {
            let Some(type_name) = link.strip_prefix("CreatureTypeExists::") else {
                continue;
            };
            if value != b"true" || !registered.contains(type_name) {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy link CreatureTypeExists::{type_name} diverges from the type registry"
                )));
            }
            flagged.insert(type_name.to_owned());
        }
        if let Some(missing) = registered.difference(&flagged).next() {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy creature type {missing} has no CreatureTypeExists flag"
            )));
        }
        Ok(capsules)
    }
}
