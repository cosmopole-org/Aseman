//! Fixture-backed transforms for legacy QuestDB build-log and signal rows.

use super::*;

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
        let capsule = aseman_contracts::legacy_realtime::store_signal_event(
            &aseman_contracts::legacy_realtime::StoreSignalPayload {
                signal_id: row.id,
                store_id: row.store_id,
                sender_id: row.user_id,
                data: row.data,
                tags,
                edited: row.edited,
            },
            *sequence,
            occurred_at_micros,
            &aseman_contracts::legacy_realtime::SignalStreamPolicy {
                authorization_scope: policy.authorization_scope.clone(),
                retention_class: policy.retention_class.clone(),
            },
        )
        .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?;
        capsules.push(capsule);
    }
    Ok(capsules)
}

pub(crate) fn decode_legacy_tags(encoded: &str) -> LegacyMigrationResult<Vec<String>> {
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
