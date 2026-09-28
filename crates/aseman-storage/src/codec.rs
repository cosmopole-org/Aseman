//! Records <-> sealed capsules.
//!
//! A record's fields are the capsule body (an object), its relations are the capsule's
//! relationships, and every write is a new sealed revision chained to the previous
//! one. A `null` or absent field is not stored.

use crate::error::{StorageError, StorageResult};
use crate::schema::{FieldType, Model, OwnerScope};
use crate::value::{Data, Id, Row, Value};
use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleRelationship, CapsuleValue,
    DIGEST_ALGORITHM, ENCODING_VERSION, OwnerScope as CapsuleOwner, StorageClass,
};
use std::collections::BTreeMap;

fn invalid(model: &Model, message: impl std::fmt::Display) -> StorageError {
    StorageError::invalid(format!("{}: {message}", model.name))
}

/// Check that every value in `data` matches its declared field or relation. With
/// `complete`, also that every required field is present.
pub fn validate(model: &Model, data: &Data, complete: bool) -> StorageResult<()> {
    for (name, value) in data {
        if value.is_null() {
            continue;
        }
        if model.relations.contains_key(name) {
            if !matches!(value, Value::Id(_)) {
                return Err(invalid(model, format!("relation {name} takes an id")));
            }
            continue;
        }
        let Some(field_type) = model.fields.get(name) else {
            return Err(invalid(model, format!("unknown field {name}")));
        };
        let matches = match field_type {
            FieldType::Text => matches!(value, Value::Text(_)),
            FieldType::Integer | FieldType::TimestampMicros => matches!(value, Value::Int(_)),
            FieldType::Float => matches!(value, Value::Float(_) | Value::Int(_)),
            FieldType::Bool => matches!(value, Value::Bool(_)),
            FieldType::Bytes => matches!(value, Value::Bytes(_)),
            FieldType::CapsuleId => matches!(value, Value::Id(_)),
            FieldType::Document => matches!(value, Value::Json(serde_json::Value::Object(_))),
        };
        if !matches {
            return Err(invalid(
                model,
                format!("field {name} takes a {} value", field_type.name()),
            ));
        }
    }
    if complete
        && let Some(missing) = model
            .required
            .iter()
            .find(|field| data.get(*field).is_none_or(Value::is_null))
    {
        return Err(invalid(model, format!("required field {missing} is missing")));
    }
    Ok(())
}

fn storage_class(model: &Model) -> StorageResult<StorageClass> {
    Ok(match model.storage_class.as_str() {
        "core" => StorageClass::Core,
        "guest_data" => StorageClass::GuestData,
        "telemetry" => StorageClass::Telemetry,
        "audit" => StorageClass::Audit,
        "finance" => StorageClass::Finance,
        "outbox" => StorageClass::Outbox,
        "realtime" => StorageClass::Realtime,
        other => return Err(invalid(model, format!("storage class {other}"))),
    })
}

fn owner(model: &Model, data: &Data) -> CapsuleOwner {
    match (model.owner_scope, &model.owner) {
        (OwnerScope::Creature, Some(relation)) => match data.get(relation) {
            Some(Value::Id(id)) => CapsuleOwner::Creature(id.0),
            _ => CapsuleOwner::Global,
        },
        _ => CapsuleOwner::Global,
    }
}

fn placeholder() -> CapsuleDigest {
    CapsuleDigest {
        algorithm: DIGEST_ALGORITHM.to_owned(),
        bytes: vec![0; 32],
    }
}

/// Seal `data` as the next revision of record `id` after `previous` (a stored
/// capsule, possibly a tombstone).
pub fn encode(
    model: &Model,
    id: Id,
    previous: Option<&CapsuleEnvelope>,
    now_micros: i64,
    data: &Data,
) -> StorageResult<CapsuleEnvelope> {
    let mut body = BTreeMap::new();
    let mut relationships = Vec::new();
    for (name, value) in data {
        if value.is_null() {
            continue;
        }
        if let Some(target) = model.relations.get(name) {
            let Value::Id(target_id) = value else {
                return Err(invalid(model, format!("relation {name} takes an id")));
            };
            relationships.push(CapsuleRelationship {
                name: name.clone(),
                target_kind: CapsuleKind(target.clone()),
                target_id: CapsuleId(target_id.0),
            });
            continue;
        }
        body.insert(name.clone(), capsule_value(value));
    }
    let created = previous.map_or(now_micros, |previous| previous.created_at_micros);
    CapsuleEnvelope {
        encoding_version: ENCODING_VERSION,
        id: CapsuleId(id.0),
        kind: CapsuleKind(model.name.clone()),
        storage_class: storage_class(model)?,
        owner_scope: owner(model, data),
        schema_version: 1,
        revision: previous.map_or(1, |previous| previous.revision + 1),
        created_at_micros: created,
        updated_at_micros: now_micros.max(created),
        previous_integrity: previous.map(|previous| previous.integrity_hash.clone()),
        integrity_hash: placeholder(),
        tombstone: false,
        relationships,
        body: Some(CapsuleValue::Object(body)),
    }
    .seal()
    .map_err(|error| invalid(model, error))
}

/// Seal the tombstone that deletes `previous`.
pub fn tombstone(
    model: &Model,
    previous: &CapsuleEnvelope,
    now_micros: i64,
) -> StorageResult<CapsuleEnvelope> {
    let mut capsule = previous.clone();
    capsule.revision += 1;
    capsule.updated_at_micros = now_micros.max(previous.updated_at_micros);
    capsule.previous_integrity = Some(previous.integrity_hash.clone());
    capsule.integrity_hash = placeholder();
    capsule.tombstone = true;
    capsule.body = None;
    capsule.seal().map_err(|error| invalid(model, error))
}

/// The record a live capsule holds.
pub fn decode(model: &Model, capsule: &CapsuleEnvelope) -> StorageResult<Row> {
    let mut data = Data::new();
    if let Some(CapsuleValue::Object(body)) = &capsule.body {
        for (name, value) in body {
            let Some(field_type) = model.fields.get(name) else {
                continue;
            };
            data.insert(name.clone(), record_value(*field_type, value));
        }
    }
    for relationship in &capsule.relationships {
        if model.relations.contains_key(&relationship.name) {
            data.insert(
                relationship.name.clone(),
                Value::Id(Id(relationship.target_id.0)),
            );
        }
    }
    Ok(Row {
        id: Id(capsule.id.0),
        revision: capsule.revision,
        created_at_micros: capsule.created_at_micros,
        updated_at_micros: capsule.updated_at_micros,
        data,
    })
}

/// A record value as a capsule value.
#[must_use]
pub fn capsule_value(value: &Value) -> CapsuleValue {
    match value {
        Value::Null => CapsuleValue::Null,
        Value::Bool(value) => CapsuleValue::Bool(*value),
        Value::Int(value) => CapsuleValue::Integer(*value),
        Value::Float(value) => CapsuleValue::Float(*value),
        Value::Text(value) => CapsuleValue::Text(value.clone()),
        Value::Bytes(value) => CapsuleValue::Bytes(value.clone()),
        Value::Id(id) => CapsuleValue::Bytes(id.0.to_vec()),
        Value::Json(json) => json_capsule(json),
    }
}

/// A stored capsule value as a record value of `field_type`.
#[must_use]
pub fn record_value(field_type: FieldType, value: &CapsuleValue) -> Value {
    match (field_type, value) {
        (_, CapsuleValue::Null) => Value::Null,
        (FieldType::CapsuleId, CapsuleValue::Bytes(bytes)) if bytes.len() == 16 => {
            let mut id = [0; 16];
            id.copy_from_slice(bytes);
            Value::Id(Id(id))
        }
        (FieldType::Document, value) => Value::Json(capsule_json(value)),
        (_, CapsuleValue::Bool(value)) => Value::Bool(*value),
        (_, CapsuleValue::Integer(value)) => Value::Int(*value),
        (_, CapsuleValue::Float(value)) => Value::Float(*value),
        (_, CapsuleValue::Text(value)) => Value::Text(value.clone()),
        (_, CapsuleValue::Bytes(value)) => Value::Bytes(value.clone()),
        (_, other) => Value::Json(capsule_json(other)),
    }
}

/// JSON as a capsule value (a document field's content).
#[must_use]
pub fn json_capsule(json: &serde_json::Value) -> CapsuleValue {
    match json {
        serde_json::Value::Null => CapsuleValue::Null,
        serde_json::Value::Bool(value) => CapsuleValue::Bool(*value),
        serde_json::Value::Number(number) => number.as_i64().map_or_else(
            || CapsuleValue::Float(number.as_f64().unwrap_or_default()),
            CapsuleValue::Integer,
        ),
        serde_json::Value::String(text) => CapsuleValue::Text(text.clone()),
        serde_json::Value::Array(items) => {
            CapsuleValue::Array(items.iter().map(json_capsule).collect())
        }
        serde_json::Value::Object(map) => CapsuleValue::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), json_capsule(value)))
                .collect(),
        ),
    }
}

/// A capsule value as JSON. Bytes, which JSON lacks, become `{"$bytes": "<hex>"}`.
#[must_use]
pub fn capsule_json(value: &CapsuleValue) -> serde_json::Value {
    match value {
        CapsuleValue::Null => serde_json::Value::Null,
        CapsuleValue::Bool(value) => serde_json::Value::Bool(*value),
        CapsuleValue::Integer(value) => serde_json::Value::from(*value),
        CapsuleValue::Float(value) => serde_json::Number::from_f64(*value)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        CapsuleValue::Text(text) => serde_json::Value::String(text.clone()),
        CapsuleValue::Bytes(bytes) => serde_json::json!({
            "$bytes": bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>()
        }),
        CapsuleValue::Array(items) => {
            serde_json::Value::Array(items.iter().map(capsule_json).collect())
        }
        CapsuleValue::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), capsule_json(value)))
                .collect(),
        ),
    }
}
