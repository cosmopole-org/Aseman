//! Filter evaluation and ordering over decoded records, with SQL semantics: a
//! comparison with `NULL` is unknown, `NOT unknown` is unknown, and only `true` keeps
//! a row. Text compares bytewise (PostgreSQL `COLLATE "C"`), `NULL` sorts last
//! ascending and first descending. Providers that filter in memory use this so every
//! provider returns the same rows in the same order.

use crate::query::{Case, Cond, Direction, Order, Where};
use crate::value::{Row, Value};
use std::cmp::Ordering;

/// Whether `row` matches `filter`.
#[must_use]
pub fn matches(filter: &Where, row: &Row) -> bool {
    evaluate(filter, row) == Some(true)
}

fn evaluate(filter: &Where, row: &Row) -> Option<bool> {
    match filter {
        Where::Field(field, cond) => condition(row.get(field), cond),
        Where::And(filters) => {
            let mut result = Some(true);
            for filter in filters {
                match evaluate(filter, row) {
                    Some(false) => return Some(false),
                    None => result = None,
                    Some(true) => {}
                }
            }
            result
        }
        Where::Or(filters) => {
            let mut result = Some(false);
            for filter in filters {
                match evaluate(filter, row) {
                    Some(true) => return Some(true),
                    None => result = None,
                    Some(false) => {}
                }
            }
            result
        }
        Where::Not(filter) => evaluate(filter, row).map(|value| !value),
    }
}

fn condition(value: &Value, cond: &Cond) -> Option<bool> {
    let text = |pattern: &str, case: Case, test: fn(&str, &str) -> bool| match value {
        Value::Null => None,
        Value::Text(text) => Some(match case {
            Case::Sensitive => test(text, pattern),
            Case::Insensitive => test(&text.to_lowercase(), &pattern.to_lowercase()),
        }),
        _ => Some(false),
    };
    match cond {
        Cond::IsNull(null) => Some(value.is_null() == *null),
        Cond::Equals(Value::Null) => Some(value.is_null()),
        Cond::Not(Value::Null) => Some(!value.is_null()),
        Cond::Equals(expected) => compare(value, expected).map(|order| order == Ordering::Equal),
        Cond::Not(expected) => compare(value, expected).map(|order| order != Ordering::Equal),
        Cond::In(values) => {
            if value.is_null() {
                return None;
            }
            Some(
                values
                    .iter()
                    .any(|candidate| compare(value, candidate) == Some(Ordering::Equal)),
            )
        }
        Cond::NotIn(values) => {
            if value.is_null() {
                return None;
            }
            Some(
                !values
                    .iter()
                    .any(|candidate| compare(value, candidate) == Some(Ordering::Equal)),
            )
        }
        Cond::Lt(bound) => compare(value, bound).map(|order| order == Ordering::Less),
        Cond::Lte(bound) => compare(value, bound).map(|order| order != Ordering::Greater),
        Cond::Gt(bound) => compare(value, bound).map(|order| order == Ordering::Greater),
        Cond::Gte(bound) => compare(value, bound).map(|order| order != Ordering::Less),
        Cond::Contains(pattern, case) => {
            text(pattern, *case, |text, pattern| text.contains(pattern))
        }
        Cond::StartsWith(pattern, case) => {
            text(pattern, *case, |text, pattern| text.starts_with(pattern))
        }
        Cond::EndsWith(pattern, case) => {
            text(pattern, *case, |text, pattern| text.ends_with(pattern))
        }
    }
}

/// Order two non-null values of comparable types; `None` for `NULL` or a mismatch.
#[must_use]
pub fn compare(left: &Value, right: &Value) -> Option<Ordering> {
    use Value::{Bool, Bytes, Float, Id, Int, Json, Text};
    match (left, right) {
        (Bool(left), Bool(right)) => Some(left.cmp(right)),
        (Int(left), Int(right)) => Some(left.cmp(right)),
        (Float(left), Float(right)) => left.partial_cmp(right),
        (Int(left), Float(right)) => (*left as f64).partial_cmp(right),
        (Float(left), Int(right)) => left.partial_cmp(&(*right as f64)),
        (Text(left), Text(right)) => Some(left.as_bytes().cmp(right.as_bytes())),
        (Bytes(left), Bytes(right)) => Some(left.cmp(right)),
        (Id(left), Id(right)) => Some(left.cmp(right)),
        (Json(left), Json(right)) => (left == right).then_some(Ordering::Equal),
        _ => None,
    }
}

/// Order two rows by `order_by`, then by id.
#[must_use]
pub fn order(left: &Row, right: &Row, order_by: &[Order]) -> Ordering {
    for order in order_by {
        let ascending = match (left.get(&order.field), right.get(&order.field)) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Greater,
            (_, Value::Null) => Ordering::Less,
            (left, right) => compare(left, right).unwrap_or(Ordering::Equal),
        };
        let ordering = match order.direction {
            Direction::Asc => ascending,
            Direction::Desc => ascending.reverse(),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.id.cmp(&right.id)
}

/// Every field or relation `filter` names.
pub fn fields<'a>(filter: &'a Where, out: &mut Vec<&'a str>) {
    match filter {
        Where::Field(field, _) => out.push(field),
        Where::And(filters) | Where::Or(filters) => {
            for filter in filters {
                fields(filter, out);
            }
        }
        Where::Not(filter) => fields(filter, out),
    }
}
