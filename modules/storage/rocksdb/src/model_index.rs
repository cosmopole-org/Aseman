//! Secondary indexes and the query planner of the RocksDB provider (ADR 0036).
//!
//! Every field that a unique index, a range index, or a relation names gets a
//! secondary index: one key per live record,
//! `aseman/capsule/index/{kind}/{field}/{value}/{id}`, where `{value}` is the hex of an
//! order-preserving encoding, so key order is value order (and the keys stay UTF-8
//! for the replicated store). A `NULL` is not indexed.
//!
//! The planner picks one index to produce candidates — equality first, then `IN`,
//! then a range or text prefix, then an ordered walk for `order_by` — and falls back
//! to scanning the model. Candidates are always re-checked with the full filter, so
//! a plan only changes cost, never results.

use aseman_storage::query::{Case, Cond, Direction, FindMany, Where};
use aseman_storage::schema::{FieldType, Model};
use aseman_storage::value::{Id, Value};

pub(crate) const INDEX_ROOT: &str = "aseman/capsule/index/";

/// The prefix of every index key of `field` of `kind`.
pub(crate) fn field_prefix(kind: &str, field: &str) -> String {
    format!("{INDEX_ROOT}{kind}/{field}/")
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| [DIGITS[usize::from(byte >> 4)], DIGITS[usize::from(byte & 15)]])
        .map(char::from)
        .collect()
}

/// Escape `0x00` so an encoded string never contains the terminator `0x00 0x00`,
/// and a text prefix encodes to a key prefix.
fn escaped(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 2);
    for byte in bytes {
        out.push(*byte);
        if *byte == 0 {
            out.push(0xff);
        }
    }
    out
}

/// The order-preserving encoding of a non-null `value` of `field_type`, without the
/// terminator (a prefix of it is the encoding of a text prefix). `None` for a value
/// the index does not hold.
fn encode_open(field_type: Option<FieldType>, value: &Value) -> Option<Vec<u8>> {
    let float = |value: f64| {
        let bits = value.to_bits();
        let ordered = if bits >> 63 == 1 {
            !bits
        } else {
            bits ^ (1 << 63)
        };
        let mut out = vec![3];
        out.extend(ordered.to_be_bytes());
        out
    };
    Some(match (field_type, value) {
        (_, Value::Null | Value::Json(_)) => return None,
        (_, Value::Bool(value)) => vec![1, u8::from(*value)],
        (Some(FieldType::Float), Value::Int(value)) => float(*value as f64),
        (_, Value::Int(value)) => {
            let mut out = vec![2];
            out.extend(((*value as u64) ^ (1 << 63)).to_be_bytes());
            out
        }
        (_, Value::Float(value)) => float(*value),
        (_, Value::Text(text)) => {
            let mut out = vec![4];
            out.extend(escaped(text.as_bytes()));
            out
        }
        (_, Value::Bytes(bytes)) => {
            let mut out = vec![5];
            out.extend(escaped(bytes));
            out
        }
        (_, Value::Id(id)) => {
            let mut out = vec![6];
            out.extend(id.0);
            out
        }
    })
}

/// The full encoding of a value (with its terminator), as index-key text.
fn encode(field_type: Option<FieldType>, value: &Value) -> Option<String> {
    let mut bytes = encode_open(field_type, value)?;
    bytes.extend([0, 0]);
    Some(hex(&bytes))
}

fn field_type(model: &Model, field: &str) -> Option<FieldType> {
    model.fields.get(field).copied()
}

/// The index key of `field` = `value` for record `id`.
pub(crate) fn index_key(model: &Model, field: &str, value: &Value, id: &Id) -> Option<String> {
    Some(format!(
        "{}{}/{}",
        field_prefix(&model.name, field),
        encode(field_type(model, field), value)?,
        hex(&id.0)
    ))
}

/// The id at the end of an index key.
pub(crate) fn key_id(key: &str) -> Option<Id> {
    let text = key.rsplit('/').next()?;
    if text.len() != 32 {
        return None;
    }
    let mut id = [0_u8; 16];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(Id(id))
}

/// Where an index scan reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Scan {
    /// Keys starting with each prefix (equality, `IN`, a text prefix).
    Prefixes(Vec<String>),
    /// Keys in `[start, end)`, in order.
    Range { start: String, end: String },
}

/// How a query produces its candidate records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Plan {
    /// Read candidates from one index.
    Index { field: String, scan: Scan },
    /// Walk the index of the single ascending `order_by` field in order, stopping once
    /// the page is full; records without a value (`NULL`, last ascending) come from a
    /// scan when the walk runs out.
    Ordered { field: String },
    /// Read every record of the model.
    Scan,
}

/// The smallest key greater than every key starting with `prefix`.
pub(crate) fn prefix_end(prefix: &str) -> String {
    let mut end = prefix.to_owned();
    end.push(char::from(0x7f));
    end
}

fn conjuncts(filter: &Where) -> Vec<&Where> {
    match filter {
        Where::And(filters) => filters.iter().flat_map(conjuncts).collect(),
        other => vec![other],
    }
}

/// The candidate scan one condition on an indexed field allows, with its rank
/// (lower is more selective).
fn scan_for(model: &Model, field: &str, cond: &Cond) -> Option<(u8, Scan)> {
    let field_type = field_type(model, field);
    let base = field_prefix(&model.name, field);
    let point = |value: &Value| encode(field_type, value).map(|value| format!("{base}{value}/"));
    let unique = model.unique.iter().any(|index| index == &[field.to_owned()]);
    match cond {
        Cond::Equals(value) if !value.is_null() => {
            Some((if unique { 0 } else { 1 }, Scan::Prefixes(vec![point(value)?])))
        }
        Cond::In(values) => {
            let points = values
                .iter()
                .filter(|value| !value.is_null())
                .map(point)
                .collect::<Option<Vec<_>>>()?;
            Some((2, Scan::Prefixes(points)))
        }
        Cond::StartsWith(text, Case::Sensitive) => {
            // The escaped text is a byte prefix of the encoding of every longer text.
            let open = encode_open(field_type, &Value::Text(text.clone()))?;
            Some((3, Scan::Prefixes(vec![format!("{base}{}", hex(&open))])))
        }
        Cond::Gt(value) | Cond::Gte(value) | Cond::Lt(value) | Cond::Lte(value) => {
            let bound = encode(field_type, value)?;
            let (start, end) = match cond {
                // A range bound is re-checked by the filter, so inclusive bounds are
                // enough for every operator.
                Cond::Gt(_) | Cond::Gte(_) => (format!("{base}{bound}"), prefix_end(&base)),
                _ => (base.clone(), prefix_end(&format!("{base}{bound}"))),
            };
            Some((4, Scan::Range { start, end }))
        }
        _ => None,
    }
}

/// Choose how to produce the candidates of `query`.
pub(crate) fn plan(model: &Model, query: &FindMany) -> Plan {
    let indexed = model.indexed();
    let mut best: Option<(u8, String, Scan)> = None;
    if let Some(filter) = &query.filter {
        for conjunct in conjuncts(filter) {
            let Where::Field(field, cond) = conjunct else {
                continue;
            };
            if !indexed.contains(field.as_str()) {
                continue;
            }
            if let Some((rank, scan)) = scan_for(model, field, cond)
                && best.as_ref().is_none_or(|(best, _, _)| rank < *best)
            {
                best = Some((rank, field.clone(), scan));
            }
        }
    }
    if let Some((_, field, scan)) = best {
        return Plan::Index { field, scan };
    }
    if let [order] = query.order_by.as_slice()
        && order.direction == Direction::Asc
        && indexed.contains(order.field.as_str())
        && query.take.is_some()
    {
        return Plan::Ordered {
            field: order.field.clone(),
        };
    }
    Plan::Scan
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_storage::query::Order;
    use aseman_storage::schema::Schema;

    #[test]
    fn encodings_sort_like_their_values() {
        let ints = [-5_i64, -1, 0, 1, 7, i64::MAX, i64::MIN];
        let mut encoded = ints
            .iter()
            .map(|value| (encode(None, &Value::Int(*value)).unwrap(), *value))
            .collect::<Vec<_>>();
        encoded.sort();
        assert_eq!(
            encoded.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
            [i64::MIN, -5, -1, 0, 1, 7, i64::MAX]
        );
        let floats = [-2.5_f64, -0.5, 0.0, 0.25, 10.0];
        let mut encoded = floats
            .iter()
            .map(|value| (encode(None, &Value::Float(*value)).unwrap(), *value))
            .collect::<Vec<_>>();
        encoded.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            encoded.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
            floats
        );
        let texts = ["", "a", "a\0", "ab", "b"];
        let mut encoded = texts
            .iter()
            .map(|value| (encode(None, &Value::from(*value)).unwrap(), *value))
            .collect::<Vec<_>>();
        encoded.sort();
        assert_eq!(
            encoded.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
            texts
        );
    }

    #[test]
    fn the_planner_prefers_the_most_selective_index() {
        let schema = Schema::catalog().unwrap();
        let user = schema.model("core.user").unwrap();
        let unique = plan(
            user,
            &FindMany::filter(Where::And(vec![
                Where::field("status", Cond::Equals(Value::from("x"))),
                Where::eq("username", "ada"),
            ])),
        );
        assert!(matches!(unique, Plan::Index { ref field, .. } if field == "username"));
        let ordered = plan(user, &FindMany::default().order_by(Order::asc("email")).take(5));
        assert_eq!(
            ordered,
            Plan::Ordered {
                field: "email".to_owned()
            }
        );
        assert_eq!(
            plan(user, &FindMany::filter(Where::eq("status", "x"))),
            Plan::Scan
        );
    }
}
