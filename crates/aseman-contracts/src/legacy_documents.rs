//! Legacy JSON documents as capsule values (ADR 0016).
//!
//! The A308 export and the capsule repositories convert with these functions, so a
//! document written after cutover is byte-identical to one migrated from legacy.

use crate::capsule::{CapsuleValue, encode_value};
use serde_json::{Map, Number, Value};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum LegacyDocumentError {
    #[error("legacy document {0} contains a number outside the capsule range")]
    NumberOutOfRange(String),
    #[error("legacy document {0} repeats member {1}")]
    RepeatedMember(String, String),
    #[error("capsule document holds a value JSON cannot express")]
    NotJson,
    #[error("{0}")]
    Encoding(String),
}

/// Convert decoded legacy JSON into a canonical capsule value without widening,
/// truncating, or reordering any member. `key` names the document in errors.
pub fn legacy_json_to_capsule_value(
    key: &str,
    value: &Value,
) -> Result<CapsuleValue, LegacyDocumentError> {
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
                return Err(LegacyDocumentError::NumberOutOfRange(key.to_owned()));
            }
        }
        Value::String(value) => CapsuleValue::Text(value.clone()),
        Value::Array(items) => CapsuleValue::Array(
            items
                .iter()
                .map(|item| legacy_json_to_capsule_value(key, item))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Value::Object(members) => {
            let mut converted = BTreeMap::new();
            for (member, value) in members {
                if converted
                    .insert(member.clone(), legacy_json_to_capsule_value(key, value)?)
                    .is_some()
                {
                    return Err(LegacyDocumentError::RepeatedMember(
                        key.to_owned(),
                        member.clone(),
                    ));
                }
            }
            CapsuleValue::Object(converted)
        }
    })
}

/// The JSON a capsule document was converted from.
pub fn capsule_value_to_json(value: &CapsuleValue) -> Result<Value, LegacyDocumentError> {
    Ok(match value {
        CapsuleValue::Null => Value::Null,
        CapsuleValue::Bool(value) => Value::Bool(*value),
        CapsuleValue::Integer(value) => Value::Number(Number::from(*value)),
        CapsuleValue::Float(value) => {
            Value::Number(Number::from_f64(*value).ok_or(LegacyDocumentError::NotJson)?)
        }
        CapsuleValue::Text(value) => Value::String(value.clone()),
        CapsuleValue::Array(items) => Value::Array(
            items
                .iter()
                .map(capsule_value_to_json)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        CapsuleValue::Object(members) => Value::Object(
            members
                .iter()
                .map(|(member, value)| Ok((member.clone(), capsule_value_to_json(value)?)))
                .collect::<Result<Map<_, _>, LegacyDocumentError>>()?,
        ),
        _ => return Err(LegacyDocumentError::NotJson),
    })
}

/// Domain-separated digest over the canonical encoding of a document value.
pub fn legacy_document_digest(document: &CapsuleValue) -> Result<Vec<u8>, LegacyDocumentError> {
    let encoded =
        encode_value(document).map_err(|error| LegacyDocumentError::Encoding(error.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(b"ASEMAN-LEGACY-DOCUMENT-DIGEST-V1\0");
    hasher.update((encoded.len() as u64).to_be_bytes());
    hasher.update(&encoded);
    Ok(hasher.finalize().to_vec())
}

/// The four document fields of an ADR 0016 capsule body: `document`,
/// `document_path`, `entry_count` (top-level members), and `content_digest`.
pub fn legacy_document_fields(
    key: &str,
    document_path: &str,
    document: &Map<String, Value>,
) -> Result<BTreeMap<String, CapsuleValue>, LegacyDocumentError> {
    let entry_count = i64::try_from(document.len()).map_err(|_| {
        LegacyDocumentError::Encoding(format!("legacy document {key} is too large"))
    })?;
    let document = legacy_json_to_capsule_value(key, &Value::Object(document.clone()))?;
    let content_digest = legacy_document_digest(&document)?;
    Ok(BTreeMap::from([
        ("document".to_owned(), document),
        (
            "document_path".to_owned(),
            CapsuleValue::Text(document_path.to_owned()),
        ),
        ("entry_count".to_owned(), CapsuleValue::Integer(entry_count)),
        (
            "content_digest".to_owned(),
            CapsuleValue::Bytes(content_digest),
        ),
    ]))
}

/// Merge `source` into `target` as legacy `put_json(.., merge = true)` does: objects
/// merge member by member, and any other value replaces what was there (`null`
/// included).
pub fn merge_legacy_objects(target: &mut Map<String, Value>, source: &Map<String, Value>) {
    for (member, value) in source {
        match (target.get_mut(member), value) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => {
                merge_legacy_objects(existing, incoming);
            }
            _ => {
                target.insert(member.clone(), value.clone());
            }
        }
    }
}

/// The records legacy `put_json(key, path, object, merge)` writes, as `(path, JSON text)`
/// pairs in write order (the node's `index_json`, ADR 0028). The object at `path` is
/// merged with its stored object when `merge` is set, then every nested object is
/// indexed at `path.member` (merged the same way) and every non-null leaf is written
/// at `path.member`. `stored` answers the currently stored object of a path.
pub fn legacy_json_index_writes(
    path: &str,
    object: &Map<String, Value>,
    merge: bool,
    stored: &dyn Fn(&str) -> Option<Map<String, Value>>,
) -> Vec<(String, String)> {
    let mut writes = Vec::new();
    index_json_into(path, object, merge, stored, &mut writes);
    writes
}

fn index_json_into(
    path: &str,
    object: &Map<String, Value>,
    merge: bool,
    stored: &dyn Fn(&str) -> Option<Map<String, Value>>,
    writes: &mut Vec<(String, String)>,
) {
    let mut merged = if merge {
        stored(path).unwrap_or_default()
    } else {
        Map::new()
    };
    merge_legacy_objects(&mut merged, object);
    writes.push((path.to_owned(), Value::Object(merged).to_string()));
    let mut members = object.keys().collect::<Vec<_>>();
    members.sort();
    for member in members {
        let child = format!("{path}.{member}");
        match &object[member] {
            Value::Null => {}
            Value::Object(nested) => index_json_into(&child, nested, merge, stored, writes),
            leaf => writes.push((child, leaf.to_string())),
        }
    }
}

/// The object at a legacy dotted `path` inside a document rooted at `root_path`, as
/// legacy `get_json` answers it: only objects are returned.
#[must_use]
pub fn legacy_document_object_at<'a>(
    root_path: &str,
    document: &'a Map<String, Value>,
    path: &str,
) -> Option<&'a Map<String, Value>> {
    if path == root_path {
        return Some(document);
    }
    let rest = path.strip_prefix(root_path)?.strip_prefix('.')?;
    let mut current = document;
    for member in rest.split('.') {
        current = current.get(member)?.as_object()?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_index_writes_match_the_legacy_record_layout() {
        let stored = |path: &str| {
            (path == "doc").then(|| {
                serde_json::json!({"keep": 1, "n": 0})
                    .as_object()
                    .cloned()
                    .unwrap()
            })
        };
        let object = serde_json::json!({"n": 3, "sub": {"x": "y"}, "gone": null});
        let writes = legacy_json_index_writes("doc", object.as_object().unwrap(), true, &stored);
        assert_eq!(
            writes,
            vec![
                (
                    "doc".to_owned(),
                    r#"{"gone":null,"keep":1,"n":3,"sub":{"x":"y"}}"#.to_owned()
                ),
                ("doc.n".to_owned(), "3".to_owned()),
                ("doc.sub".to_owned(), r#"{"x":"y"}"#.to_owned()),
                ("doc.sub.x".to_owned(), r#""y""#.to_owned()),
            ]
        );
        // Without merge the stored object is replaced.
        let replaced = legacy_json_index_writes("doc", object.as_object().unwrap(), false, &stored);
        assert_eq!(replaced[0].1, r#"{"gone":null,"n":3,"sub":{"x":"y"}}"#);
    }

    #[test]
    fn documents_round_trip_and_paths_resolve_like_legacy_get_json() {
        let document = serde_json::json!({
            "public": {"profile": {"name": "a", "age": 3, "ratio": 0.5}},
            "tags": ["x", null],
            "leaf": true
        });
        let Value::Object(document) = document else {
            unreachable!()
        };
        let fields = legacy_document_fields("CreatMeta::1", "metadata", &document).unwrap();
        assert_eq!(fields["entry_count"], CapsuleValue::Integer(3));
        assert_eq!(
            capsule_value_to_json(&fields["document"]).unwrap(),
            Value::Object(document.clone())
        );
        let profile = legacy_document_object_at("metadata", &document, "metadata.public.profile");
        assert_eq!(profile.unwrap()["name"], "a");
        assert!(legacy_document_object_at("metadata", &document, "metadata.leaf").is_none());
        assert!(legacy_document_object_at("metadata", &document, "metadata.none").is_none());
        assert!(legacy_document_object_at("metadata", &document, "other").is_none());
        let mut merged = serde_json::json!({"a": {"x": 1, "y": 2}, "b": 1});
        let Value::Object(target) = &mut merged else {
            unreachable!()
        };
        let Value::Object(source) = serde_json::json!({"a": {"y": 3}, "b": {"z": 1}, "c": null})
        else {
            unreachable!()
        };
        merge_legacy_objects(target, &source);
        assert_eq!(
            merged,
            serde_json::json!({"a": {"x": 1, "y": 3}, "b": {"z": 1}, "c": null})
        );
        let huge = serde_json::json!({"n": u64::MAX});
        assert!(legacy_json_to_capsule_value("k", &huge).is_err());
    }
}
