//! Capsule layouts (ADR 0034).
//!
//! In the flattened layout (the default, capsule mode off) a capsule is an ordinary
//! row: every declared field is its own typed column, a document field is a JSONB
//! column named after the field, and relationships are UUID columns. The envelope is
//! rebuilt from the row on read and verified against the stored integrity hash, so a
//! row that was edited outside the provider is refused. `capsule_shape` keeps the few
//! facts columns cannot: which fields were explicitly `null`, which float fields held
//! an integer, and a non-canonical relationship order. It is `NULL` for most rows.
//!
//! In capsule mode the canonical envelope is packed into `capsule_cbor`, beside the
//! same typed columns queries use. Reads accept a row of either layout, so switching
//! modes never strands data; [`convert_rows`] rewrites the mutable tables in place.
//!
//! The provider owns its columns: [`reconcile_columns`] adds a column the mapping
//! gained, changes a column whose mapped type changed, and relaxes `capsule_cbor` on a
//! schema created before the flattened layout existed. It never drops a column.

use crate::{
    PostgresStorageError, SCHEMA, SqlParam, StorageResult, TableMapping, all_tables,
    capsule_values, map_postgres_error, qualified, sql_identifier, sql_parameters,
};
use aseman_config::CapsuleLayout;
use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleRelationship, CapsuleValue,
    DIGEST_ALGORITHM, ENCODING_VERSION, OwnerScope, StorageClass,
};
use postgres::GenericClient;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub(crate) const CBOR_COLUMN: &str = "capsule_cbor";
pub(crate) const SHAPE_COLUMN: &str = "capsule_shape";

/// The envelope columns of every mapped table, in the order rows are selected and
/// written. Field, document, and relationship columns follow them.
pub(crate) const ENVELOPE_COLUMNS: [&str; 13] = [
    "id",
    "schema_version",
    "revision",
    "created_at_micros",
    "updated_at_micros",
    "previous_integrity",
    "integrity_hash",
    "owner_type",
    "owner_id",
    "owner_name",
    "tombstone",
    CBOR_COLUMN,
    SHAPE_COLUMN,
];

const CONVERT_BATCH: i64 = 256;

/// What the flattened columns cannot say on their own.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Shape {
    /// Body fields present with an explicit `null` (an absent field is also `NULL`).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) null: BTreeSet<String>,
    /// Float fields that held an exact integer.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) integer: BTreeSet<String>,
    /// Relationship names in envelope order, when that is not name order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) relationships: Vec<String>,
}

impl Shape {
    pub(crate) fn of(mapping: &TableMapping, capsule: &CapsuleEnvelope) -> Self {
        let mut shape = Self::default();
        if let Some(CapsuleValue::Object(body)) = &capsule.body {
            for (name, field_type) in &mapping.fields {
                match body.get(name) {
                    Some(CapsuleValue::Null) => {
                        shape.null.insert(name.clone());
                    }
                    Some(CapsuleValue::Integer(_)) if field_type == "float" => {
                        shape.integer.insert(name.clone());
                    }
                    _ => {}
                }
            }
        }
        let order = capsule
            .relationships
            .iter()
            .map(|relationship| relationship.name.clone())
            .collect::<Vec<_>>();
        if order.windows(2).any(|pair| pair[0] > pair[1]) {
            shape.relationships = order;
        }
        shape
    }

    pub(crate) fn to_column(&self) -> Option<Value> {
        if self.null.is_empty() && self.integer.is_empty() && self.relationships.is_empty() {
            None
        } else {
            serde_json::to_value(self).ok()
        }
    }
}

/// Every column of `mapping`, in the order [`envelope_from_row`] reads them.
pub(crate) fn select_list(mapping: &TableMapping) -> String {
    ENVELOPE_COLUMNS
        .iter()
        .map(|column| (*column).to_owned())
        .chain(
            mapping
                .fields
                .keys()
                .map(|name| sql_identifier(&mapping.field_columns[name])),
        )
        .chain(
            mapping
                .document_fields
                .iter()
                .map(|name| sql_identifier(name)),
        )
        .chain(
            mapping
                .relationships
                .keys()
                .map(|name| sql_identifier(name)),
        )
        .collect::<Vec<_>>()
        .join(", ")
}

/// How many columns [`select_list`] selects; extra selected values follow them.
pub(crate) fn select_width(mapping: &TableMapping) -> usize {
    ENVELOPE_COLUMNS.len()
        + mapping.fields.len()
        + mapping.document_fields.len()
        + mapping.relationships.len()
}

fn invalid(message: impl Into<String>) -> PostgresStorageError {
    PostgresStorageError::Invalid(message.into())
}

fn column<'a, T: postgres::types::FromSql<'a>>(
    row: &'a postgres::Row,
    index: usize,
) -> StorageResult<T> {
    row.try_get(index)
        .map_err(|error| invalid(format!("stored capsule column {index}: {error}")))
}

/// Rebuild the envelope of one row selected with [`select_list`]: decode a packed
/// capsule, or assemble a flattened one from its columns. Either way the result must
/// verify against its stored integrity hash.
pub(crate) fn envelope_from_row(
    mapping: &TableMapping,
    row: &postgres::Row,
) -> StorageResult<CapsuleEnvelope> {
    if let Some(bytes) = column::<Option<Vec<u8>>>(row, 11)? {
        return CapsuleEnvelope::from_canonical_bytes(&bytes)
            .map_err(|error| invalid(error.to_string()));
    }
    let shape = match column::<Option<Value>>(row, 12)? {
        Some(value) => serde_json::from_value::<Shape>(value)
            .map_err(|error| invalid(format!("stored capsule shape: {error}")))?,
        None => Shape::default(),
    };
    let id = column::<Uuid>(row, 0)?;
    let tombstone = column::<bool>(row, 10)?;
    let owner_id = column::<Option<Uuid>>(row, 8)?.map(|id| *id.as_bytes());
    let owner_scope = match (column::<String>(row, 7)?.as_str(), owner_id) {
        ("global", None) => OwnerScope::Global,
        ("node", Some(id)) => OwnerScope::Node(id),
        ("creature", Some(id)) => OwnerScope::Creature(id),
        ("module", None) => OwnerScope::Module(
            column::<Option<String>>(row, 9)?.ok_or_else(|| invalid("module owner has no name"))?,
        ),
        _ => return Err(invalid("stored owner scope is inconsistent")),
    };
    let digest = |bytes: Vec<u8>| CapsuleDigest {
        algorithm: DIGEST_ALGORITHM.to_owned(),
        bytes,
    };

    let mut index = ENVELOPE_COLUMNS.len();
    let mut body = BTreeMap::new();
    for (name, field_type) in &mapping.fields {
        let value = read_field(row, index, field_type, shape.integer.contains(name))?;
        index += 1;
        match value {
            Some(value) => {
                body.insert(name.clone(), value);
            }
            None if shape.null.contains(name) => {
                body.insert(name.clone(), CapsuleValue::Null);
            }
            None => {}
        }
    }
    for name in &mapping.document_fields {
        if let Some(value) = column::<Option<Value>>(row, index)? {
            body.insert(name.clone(), document_value(&value)?);
        }
        index += 1;
    }
    let mut relationships = BTreeMap::new();
    for (name, relationship) in &mapping.relationships {
        if let Some(target) = column::<Option<Uuid>>(row, index)? {
            relationships.insert(
                name.clone(),
                CapsuleRelationship {
                    name: name.clone(),
                    target_kind: CapsuleKind(relationship.target_kind.clone()),
                    target_id: CapsuleId(*target.as_bytes()),
                },
            );
        }
        index += 1;
    }
    let relationships = if shape.relationships.is_empty() {
        relationships.into_values().collect()
    } else {
        shape
            .relationships
            .iter()
            .map(|name| {
                relationships
                    .remove(name)
                    .ok_or_else(|| invalid("stored relationship order names a missing column"))
            })
            .collect::<StorageResult<Vec<_>>>()?
    };

    let envelope = CapsuleEnvelope {
        encoding_version: ENCODING_VERSION,
        id: CapsuleId(*id.as_bytes()),
        kind: CapsuleKind(mapping.kind.clone()),
        storage_class: storage_class(&mapping.storage_class)?,
        owner_scope,
        schema_version: u32::try_from(column::<i32>(row, 1)?)
            .map_err(|_| invalid("stored schema version is negative"))?,
        revision: u64::try_from(column::<i64>(row, 2)?)
            .map_err(|_| invalid("stored revision is negative"))?,
        created_at_micros: column(row, 3)?,
        updated_at_micros: column(row, 4)?,
        previous_integrity: column::<Option<Vec<u8>>>(row, 5)?.map(digest),
        integrity_hash: digest(column(row, 6)?),
        tombstone,
        relationships,
        body: (!tombstone).then_some(CapsuleValue::Object(body)),
    };
    envelope
        .verify()
        .map_err(|error| invalid(format!("stored row does not match its capsule: {error}")))?;
    Ok(envelope)
}

fn read_field(
    row: &postgres::Row,
    index: usize,
    field_type: &str,
    integer_in_float: bool,
) -> StorageResult<Option<CapsuleValue>> {
    Ok(match field_type {
        "bool" => column::<Option<bool>>(row, index)?.map(CapsuleValue::Bool),
        "integer" | "timestamp_micros" => {
            column::<Option<i64>>(row, index)?.map(CapsuleValue::Integer)
        }
        "float" => column::<Option<f64>>(row, index)?.map(|value| {
            if integer_in_float {
                CapsuleValue::Integer(value as i64)
            } else {
                CapsuleValue::Float(value)
            }
        }),
        "bytes" => column::<Option<Vec<u8>>>(row, index)?.map(CapsuleValue::Bytes),
        "text" => column::<Option<String>>(row, index)?.map(CapsuleValue::Text),
        "capsule_id" => column::<Option<Uuid>>(row, index)?
            .map(|id| CapsuleValue::Bytes(id.as_bytes().to_vec())),
        other => return Err(invalid(format!("unknown field type {other}"))),
    })
}

fn storage_class(name: &str) -> StorageResult<StorageClass> {
    Ok(match name {
        "core" => StorageClass::Core,
        "guest_data" => StorageClass::GuestData,
        "telemetry" => StorageClass::Telemetry,
        "audit" => StorageClass::Audit,
        "finance" => StorageClass::Finance,
        "outbox" => StorageClass::Outbox,
        "realtime" => StorageClass::Realtime,
        other => {
            return Err(invalid(format!(
                "storage class {other} has no table layout"
            )));
        }
    })
}

/// A capsule value as JSONB, losslessly: JSON has no bytes, no float/integer split,
/// and JSONB refuses NUL, so those travel as single-key `$`-tagged objects. An object
/// whose own keys could be mistaken for a tag travels as `$entries`.
pub(crate) fn document_json(value: &CapsuleValue) -> Value {
    let tagged = |tag: &str, value: Value| Value::Object(Map::from_iter([(tag.to_owned(), value)]));
    match value {
        CapsuleValue::Null => Value::Null,
        CapsuleValue::Bool(value) => Value::Bool(*value),
        CapsuleValue::Integer(value) => Value::from(*value),
        CapsuleValue::Float(value) => tagged("$float", Value::String(format!("{value:?}"))),
        CapsuleValue::Bytes(bytes) => tagged("$bytes", Value::String(hex(bytes))),
        CapsuleValue::Text(text) if text.contains('\0') => {
            tagged("$text", Value::String(hex(text.as_bytes())))
        }
        CapsuleValue::Text(text) => Value::String(text.clone()),
        CapsuleValue::Array(items) => Value::Array(items.iter().map(document_json).collect()),
        CapsuleValue::Object(map)
            if map
                .keys()
                .any(|key| key.starts_with('$') || key.contains('\0')) =>
        {
            tagged(
                "$entries",
                Value::Array(
                    map.iter()
                        .map(|(key, value)| {
                            Value::Array(vec![
                                Value::String(hex(key.as_bytes())),
                                document_json(value),
                            ])
                        })
                        .collect(),
                ),
            )
        }
        CapsuleValue::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), document_json(value)))
                .collect(),
        ),
    }
}

/// The inverse of [`document_json`].
pub(crate) fn document_value(value: &Value) -> StorageResult<CapsuleValue> {
    Ok(match value {
        Value::Null => CapsuleValue::Null,
        Value::Bool(value) => CapsuleValue::Bool(*value),
        Value::Number(number) => CapsuleValue::Integer(
            number
                .as_i64()
                .ok_or_else(|| invalid("a stored document number is not an integer"))?,
        ),
        Value::String(text) => CapsuleValue::Text(text.clone()),
        Value::Array(items) => CapsuleValue::Array(
            items
                .iter()
                .map(document_value)
                .collect::<StorageResult<_>>()?,
        ),
        Value::Object(map) => {
            let tag = map
                .iter()
                .next()
                .filter(|(key, _)| map.len() == 1 && key.starts_with('$'));
            match tag {
                Some((tag, Value::String(text))) if tag == "$float" => CapsuleValue::Float(
                    text.parse()
                        .map_err(|_| invalid("a stored document float is malformed"))?,
                ),
                Some((tag, Value::String(text))) if tag == "$bytes" => {
                    CapsuleValue::Bytes(unhex(text)?)
                }
                Some((tag, Value::String(text))) if tag == "$text" => CapsuleValue::Text(
                    String::from_utf8(unhex(text)?)
                        .map_err(|_| invalid("a stored document text is not UTF-8"))?,
                ),
                Some((tag, Value::Array(entries))) if tag == "$entries" => {
                    let mut object = BTreeMap::new();
                    for entry in entries {
                        let Some([Value::String(key), value]) = entry.as_array().map(Vec::as_slice)
                        else {
                            return Err(invalid("a stored document entry is malformed"));
                        };
                        let key = String::from_utf8(unhex(key)?)
                            .map_err(|_| invalid("a stored document key is not UTF-8"))?;
                        object.insert(key, document_value(value)?);
                    }
                    CapsuleValue::Object(object)
                }
                Some(_) => return Err(invalid("a stored document carries an unknown tag")),
                None => {
                    if map.keys().any(|key| key.starts_with('$')) {
                        return Err(invalid("a stored document key is reserved"));
                    }
                    CapsuleValue::Object(
                        map.iter()
                            .map(|(key, value)| Ok((key.clone(), document_value(value)?)))
                            .collect::<StorageResult<_>>()?,
                    )
                }
            }
        }
    })
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    text
}

fn unhex(text: &str) -> StorageResult<Vec<u8>> {
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(invalid("a stored document hex value is malformed")),
    };
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(invalid("a stored document hex value is malformed"));
    }
    bytes
        .chunks(2)
        .map(|pair| Ok(digit(pair[0])? << 4 | digit(pair[1])?))
        .collect()
}

fn sql_type(field_type: &str) -> StorageResult<(&'static str, &'static str)> {
    // (DDL type, information_schema.data_type)
    Ok(match field_type {
        "bool" => ("BOOLEAN", "boolean"),
        "integer" | "timestamp_micros" => ("BIGINT", "bigint"),
        "float" => ("DOUBLE PRECISION", "double precision"),
        "bytes" => ("BYTEA", "bytea"),
        "text" => ("TEXT", "text"),
        "capsule_id" => ("UUID", "uuid"),
        other => return Err(invalid(format!("unknown field type {other}"))),
    })
}

/// Bring every existing mapped table's columns in line with the mapping. Tables that
/// do not exist yet are created whole by the generated DDL.
pub(crate) fn reconcile_columns(client: &mut impl GenericClient) -> StorageResult<()> {
    for mapping in all_tables()? {
        let existing = client
            .query(
                "SELECT column_name, data_type, is_nullable = 'YES' FROM information_schema.columns \
                 WHERE table_schema = $1 AND table_name = $2",
                &[&mapping.schema, &mapping.table],
            )
            .map_err(map_postgres_error)?
            .into_iter()
            .map(|row| (row.get::<_, String>(0), (row.get::<_, String>(1), row.get::<_, bool>(2))))
            .collect::<BTreeMap<_, _>>();
        if existing.is_empty() {
            continue;
        }
        let mut expected = vec![(SHAPE_COLUMN.to_owned(), ("JSONB", "jsonb"))];
        for (name, field_type) in &mapping.fields {
            expected.push((mapping.field_columns[name].clone(), sql_type(field_type)?));
        }
        for name in &mapping.document_fields {
            expected.push((name.clone(), ("JSONB", "jsonb")));
        }
        for name in mapping.relationships.keys() {
            // A relationship added to a populated table starts nullable; the generated
            // DDL then adds its foreign key.
            expected.push((name.clone(), ("UUID", "uuid")));
        }
        let mut changes = Vec::new();
        if existing
            .get(CBOR_COLUMN)
            .is_some_and(|(_, nullable)| !nullable)
        {
            changes.push(format!("ALTER COLUMN {CBOR_COLUMN} DROP NOT NULL"));
        }
        for (name, (ddl_type, info_type)) in expected {
            let quoted = sql_identifier(&name);
            match existing.get(&name) {
                None => changes.push(format!("ADD COLUMN IF NOT EXISTS {quoted} {ddl_type}")),
                Some((actual, _)) if actual != info_type => changes.push(format!(
                    "ALTER COLUMN {quoted} TYPE {ddl_type} USING {quoted}::{ddl_type}"
                )),
                Some(_) => {}
            }
        }
        if !changes.is_empty() {
            client
                .batch_execute(&format!(
                    "ALTER TABLE {} {}",
                    qualified(mapping),
                    changes.join(", ")
                ))
                .map_err(map_postgres_error)?;
        }
    }
    Ok(())
}

/// The layout the database was last migrated to (flattened when never recorded).
pub(crate) fn recorded_layout(client: &mut impl GenericClient) -> StorageResult<CapsuleLayout> {
    let present: bool = client
        .query_one(
            &format!("SELECT to_regclass('{SCHEMA}.storage_layout') IS NOT NULL"),
            &[],
        )
        .map_err(map_postgres_error)?
        .get(0);
    if !present {
        return Ok(CapsuleLayout::Flattened);
    }
    let layout = client
        .query_opt(&format!("SELECT layout FROM {SCHEMA}.storage_layout"), &[])
        .map_err(map_postgres_error)?
        .map(|row| row.get::<_, String>(0));
    Ok(match layout.as_deref() {
        Some("capsule") => CapsuleLayout::Capsule,
        _ => CapsuleLayout::Flattened,
    })
}

pub(crate) fn record_layout(
    client: &mut impl GenericClient,
    layout: CapsuleLayout,
) -> StorageResult<()> {
    client
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.storage_layout (singleton, layout) VALUES (TRUE, $1) \
                 ON CONFLICT (singleton) DO UPDATE SET layout = EXCLUDED.layout, changed_at = now() \
                 WHERE {SCHEMA}.storage_layout.layout <> EXCLUDED.layout"
            ),
            &[&layout_name(layout)],
        )
        .map(|_| ())
        .map_err(map_postgres_error)
}

pub(crate) fn layout_name(layout: CapsuleLayout) -> &'static str {
    match layout {
        CapsuleLayout::Flattened => "flattened",
        CapsuleLayout::Capsule => "capsule",
    }
}

/// Rewrite every row of the mutable tables that is not in `layout`. Append-only rows
/// cannot be rewritten (their tables refuse updates) and stay readable as they are.
pub(crate) fn convert_rows(
    client: &mut impl GenericClient,
    layout: CapsuleLayout,
) -> StorageResult<u64> {
    let pending = match layout {
        CapsuleLayout::Flattened => "capsule_cbor IS NOT NULL",
        CapsuleLayout::Capsule => "capsule_cbor IS NULL",
    };
    let mut converted = 0;
    for mapping in all_tables()? {
        if mapping.append_only {
            continue;
        }
        loop {
            let rows = client
                .query(
                    &format!(
                        "SELECT {} FROM {} WHERE {pending} ORDER BY id LIMIT {CONVERT_BATCH}",
                        select_list(mapping),
                        qualified(mapping)
                    ),
                    &[],
                )
                .map_err(map_postgres_error)?;
            if rows.is_empty() {
                break;
            }
            for row in &rows {
                let envelope = envelope_from_row(mapping, row)?;
                let (columns, values) = capsule_values(mapping, &envelope, layout)?;
                rewrite(client, mapping, &columns, &values)?;
                converted += 1;
            }
        }
    }
    Ok(converted)
}

fn rewrite(
    client: &mut impl GenericClient,
    mapping: &TableMapping,
    columns: &[String],
    values: &[SqlParam],
) -> StorageResult<()> {
    let assignments = columns
        .iter()
        .enumerate()
        .skip(1)
        .map(|(index, column)| format!("{} = ${}", sql_identifier(column), index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    client
        .execute(
            &format!(
                "UPDATE {} SET {assignments} WHERE id = $1",
                qualified(mapping)
            ),
            &sql_parameters(values),
        )
        .map(|_| ())
        .map_err(map_postgres_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_round_trip_through_json_losslessly() {
        let value = CapsuleValue::Object(BTreeMap::from([
            ("plain".to_owned(), CapsuleValue::Text("text".to_owned())),
            ("nul".to_owned(), CapsuleValue::Text("a\0b".to_owned())),
            ("float".to_owned(), CapsuleValue::Float(1e20)),
            ("whole".to_owned(), CapsuleValue::Float(2.0)),
            ("integer".to_owned(), CapsuleValue::Integer(i64::MIN)),
            ("bytes".to_owned(), CapsuleValue::Bytes(vec![0, 255, 16])),
            ("null".to_owned(), CapsuleValue::Null),
            (
                "list".to_owned(),
                CapsuleValue::Array(vec![CapsuleValue::Bool(true), CapsuleValue::Integer(7)]),
            ),
            (
                "tricky".to_owned(),
                CapsuleValue::Object(BTreeMap::from([(
                    "$float".to_owned(),
                    CapsuleValue::Text("not a float".to_owned()),
                )])),
            ),
        ]));
        let json = document_json(&value);
        // JSONB normalizes number text, so the test goes through a text round trip.
        let reparsed: Value = serde_json::from_str(&json.to_string()).unwrap();
        assert_eq!(document_value(&reparsed).unwrap(), value);
        assert_eq!(json["plain"], Value::String("text".to_owned()));
        assert!(document_value(&serde_json::json!({"$unknown": "x"})).is_err());
        assert!(document_value(&serde_json::json!({"$float": "x", "b": 1})).is_err());
    }
}
