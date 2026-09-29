//! Model queries (ADR 0036) compiled to SQL over the mapped tables.
//!
//! Semantics match the storage engine's evaluator: SQL three-valued logic, text
//! compared and ordered bytewise (`COLLATE "C"`), `NULL` last ascending and first
//! descending, then id. Every value is a bound parameter; every identifier comes from
//! the generated mapping.

use crate::{
    PostgresStorageError, SortValue, SqlParam, StorageResult, TableMapping, field_value, layout,
    map_postgres_error, qualified, sql_identifier, sql_parameters, table_mapping,
};
use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleKind};
use aseman_storage::query::{Case, Cond, Direction, FindMany, Where};
use aseman_storage::value::Value;
use postgres::GenericClient;
use uuid::Uuid;

/// Rows a query without `take` returns at most.
pub const DEFAULT_TAKE: u64 = aseman_contracts::capsule::MAX_QUERY_LIMIT as u64;
const MAX_DEPTH: usize = 16;

fn invalid(message: impl Into<String>) -> PostgresStorageError {
    PostgresStorageError::Invalid(message.into())
}

/// A filterable column of `mapping`: `(SQL expression, field type, is text)`.
fn column(mapping: &TableMapping, field: &str) -> StorageResult<(String, &'static str, bool)> {
    if field == "id" {
        return Ok(("id".to_owned(), "capsule_id", false));
    }
    if mapping.relationships.contains_key(field) {
        return Ok((sql_identifier(field), "capsule_id", false));
    }
    if mapping.document_fields.contains(field) {
        return Err(invalid(format!(
            "{}: document field {field} is not filterable or sortable",
            mapping.kind
        )));
    }
    let field_type = mapping
        .fields
        .get(field)
        .ok_or_else(|| invalid(format!("{}: unknown field {field}", mapping.kind)))?;
    let static_type: &'static str = match field_type.as_str() {
        "bool" => "bool",
        "integer" => "integer",
        "timestamp_micros" => "timestamp_micros",
        "float" => "float",
        "bytes" => "bytes",
        "text" => "text",
        "capsule_id" => "capsule_id",
        other => return Err(invalid(format!("unknown field type {other}"))),
    };
    Ok((
        sql_identifier(&mapping.field_columns[field]),
        static_type,
        static_type == "text",
    ))
}

fn param(field_type: &str, value: &Value) -> StorageResult<SqlParam> {
    if let (Value::Id(id), "capsule_id") = (value, field_type) {
        return Ok(SqlParam::Uuid(Some(Uuid::from_bytes(id.0))));
    }
    field_value(
        field_type,
        Some(&aseman_storage::codec::capsule_value(value)),
    )
}

fn like_pattern(text: &str, prefix: bool, suffix: bool) -> String {
    let mut pattern = String::with_capacity(text.len() + 2);
    if prefix {
        pattern.push('%');
    }
    for character in text.chars() {
        if matches!(character, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    if suffix {
        pattern.push('%');
    }
    pattern
}

fn compile(
    mapping: &TableMapping,
    filter: &Where,
    params: &mut Vec<SqlParam>,
    depth: usize,
) -> StorageResult<String> {
    if depth > MAX_DEPTH {
        return Err(invalid("filter nesting exceeds the limit"));
    }
    match filter {
        Where::And(filters) | Where::Or(filters) => {
            if filters.is_empty() {
                return Ok(if matches!(filter, Where::And(_)) {
                    "TRUE"
                } else {
                    "FALSE"
                }
                .to_owned());
            }
            let join = if matches!(filter, Where::And(_)) {
                " AND "
            } else {
                " OR "
            };
            let parts = filters
                .iter()
                .map(|filter| compile(mapping, filter, params, depth + 1))
                .collect::<StorageResult<Vec<_>>>()?;
            Ok(format!("({})", parts.join(join)))
        }
        Where::Not(filter) => Ok(format!(
            "NOT ({})",
            compile(mapping, filter, params, depth + 1)?
        )),
        Where::Field(field, cond) => {
            let (column, field_type, text) = column(mapping, field)?;
            let collated = if text {
                format!("{column} COLLATE \"C\"")
            } else {
                column.clone()
            };
            let bind = |value: &Value, params: &mut Vec<SqlParam>| -> StorageResult<String> {
                params.push(param(field_type, value)?);
                Ok(format!("${}", params.len()))
            };
            let text_match = |pattern: String, case: Case, params: &mut Vec<SqlParam>| {
                params.push(SqlParam::Text(Some(pattern)));
                let placeholder = format!("${}", params.len());
                if !text {
                    return Err(invalid(format!("{field} is not a text field")));
                }
                Ok(match case {
                    Case::Sensitive => format!("{column} LIKE {placeholder} ESCAPE '\\'"),
                    Case::Insensitive => {
                        format!("lower({column}) LIKE lower({placeholder}) ESCAPE '\\'")
                    }
                })
            };
            Ok(match cond {
                Cond::IsNull(true) | Cond::Equals(Value::Null) => format!("{column} IS NULL"),
                Cond::IsNull(false) | Cond::Not(Value::Null) => format!("{column} IS NOT NULL"),
                Cond::Equals(value) => format!("{column} = {}", bind(value, params)?),
                Cond::Not(value) => format!("{column} <> {}", bind(value, params)?),
                Cond::Lt(value) => format!("{collated} < {}", bind(value, params)?),
                Cond::Lte(value) => format!("{collated} <= {}", bind(value, params)?),
                Cond::Gt(value) => format!("{collated} > {}", bind(value, params)?),
                Cond::Gte(value) => format!("{collated} >= {}", bind(value, params)?),
                Cond::In(values) | Cond::NotIn(values) => {
                    let negated = matches!(cond, Cond::NotIn(_));
                    let listed = values
                        .iter()
                        .filter(|value| !value.is_null())
                        .collect::<Vec<_>>();
                    if listed.is_empty() {
                        // `x IN ()` is false and `x NOT IN ()` true, except for NULL.
                        return Ok(if negated {
                            format!("{column} IS NOT NULL")
                        } else {
                            format!("({column} IS NOT NULL AND FALSE)")
                        });
                    }
                    let placeholders = listed
                        .into_iter()
                        .map(|value| bind(value, params))
                        .collect::<StorageResult<Vec<_>>>()?;
                    format!(
                        "{column} {}IN ({})",
                        if negated { "NOT " } else { "" },
                        placeholders.join(", ")
                    )
                }
                Cond::Contains(text, case) => {
                    text_match(like_pattern(text, true, true), *case, params)?
                }
                Cond::StartsWith(text, case) => {
                    text_match(like_pattern(text, false, true), *case, params)?
                }
                Cond::EndsWith(text, case) => {
                    text_match(like_pattern(text, true, false), *case, params)?
                }
            })
        }
    }
}

fn filter_sql(
    mapping: &TableMapping,
    filter: Option<&Where>,
    params: &mut Vec<SqlParam>,
) -> StorageResult<String> {
    let mut clauses = vec!["NOT tombstone".to_owned()];
    if let Some(filter) = filter {
        clauses.push(compile(mapping, filter, params, 1)?);
    }
    Ok(clauses.join(" AND "))
}

/// Live capsules matching `query`, with their sort keys (then id) for merging shard
/// results. `offset` and `limit` override the query's `skip` and `take` (a shard
/// returns its first `skip + take` rows and the merge applies `skip`).
pub(crate) fn find_keyed_on(
    client: &mut impl GenericClient,
    kind: &str,
    query: &FindMany,
    offset: u64,
    limit: u64,
) -> StorageResult<Vec<(Vec<SortValue>, CapsuleEnvelope)>> {
    let mapping = table_mapping(&CapsuleKind(kind.to_owned()))?;
    let mut params = Vec::new();
    let filter = filter_sql(mapping, query.filter.as_ref(), &mut params)?;
    let mut order = Vec::new();
    let mut keys = Vec::new();
    for sort in &query.order_by {
        let (column, _, text) = column(mapping, &sort.field)?;
        let expression = if text {
            format!("{column} COLLATE \"C\"")
        } else {
            column
        };
        let direction = match sort.direction {
            Direction::Asc => "ASC",
            Direction::Desc => "DESC",
        };
        order.push(format!("{expression} {direction}"));
        keys.push(expression);
    }
    order.push("id ASC".to_owned());
    keys.push("id::text".to_owned());
    params.push(SqlParam::I64(Some(
        i64::try_from(limit).unwrap_or(i64::MAX),
    )));
    let limit_parameter = params.len();
    params.push(SqlParam::I64(Some(
        i64::try_from(offset).unwrap_or(i64::MAX),
    )));
    let offset_parameter = params.len();
    let statement = format!(
        "SELECT {}, {} FROM {} WHERE {filter} ORDER BY {} LIMIT ${limit_parameter} OFFSET ${offset_parameter}",
        layout::select_list(mapping),
        keys.iter()
            .enumerate()
            .map(|(index, key)| format!("{key} AS aseman_sort_{index}"))
            .collect::<Vec<_>>()
            .join(", "),
        qualified(mapping),
        order.join(", ")
    );
    let rows = client
        .query(&statement, &sql_parameters(&params))
        .map_err(map_postgres_error)?;
    let width = layout::select_width(mapping);
    rows.iter()
        .map(|row| {
            let envelope = layout::envelope_from_row(mapping, row)?;
            let keys = (width..row.len())
                .map(|index| SortValue::read(row, index))
                .collect();
            Ok((keys, envelope))
        })
        .collect()
}

/// Live capsules matching `query`, after `skip`, at most `take`.
pub(crate) fn find_on(
    client: &mut impl GenericClient,
    kind: &str,
    query: &FindMany,
) -> StorageResult<Vec<CapsuleEnvelope>> {
    Ok(find_keyed_on(
        client,
        kind,
        query,
        query.skip,
        query.take.unwrap_or(DEFAULT_TAKE).min(DEFAULT_TAKE),
    )?
    .into_iter()
    .map(|(_, capsule)| capsule)
    .collect())
}

/// How many live capsules match `filter`.
pub(crate) fn count_on(
    client: &mut impl GenericClient,
    kind: &str,
    filter: Option<&Where>,
) -> StorageResult<u64> {
    let mapping = table_mapping(&CapsuleKind(kind.to_owned()))?;
    let mut params = Vec::new();
    let filter = filter_sql(mapping, filter, &mut params)?;
    let count: i64 = client
        .query_one(
            &format!("SELECT count(*) FROM {} WHERE {filter}", qualified(mapping)),
            &sql_parameters(&params),
        )
        .map_err(map_postgres_error)?
        .get(0);
    Ok(u64::try_from(count).unwrap_or_default())
}

/// Merge shard results (each already ordered) by their sort keys, then apply
/// `skip` and `take`.
pub(crate) fn merge(
    mut rows: Vec<(Vec<SortValue>, CapsuleEnvelope)>,
    query: &FindMany,
) -> Vec<CapsuleEnvelope> {
    let descending = query
        .order_by
        .iter()
        .map(|sort| sort.direction == Direction::Desc)
        .chain(std::iter::once(false))
        .collect::<Vec<_>>();
    rows.sort_by(|(left, _), (right, _)| {
        for ((left, right), descending) in left.iter().zip(right).zip(&descending) {
            let order = left.compare(right);
            let order = if *descending { order.reverse() } else { order };
            if order != std::cmp::Ordering::Equal {
                return order;
            }
        }
        std::cmp::Ordering::Equal
    });
    rows.into_iter()
        .skip(usize::try_from(query.skip).unwrap_or(usize::MAX))
        .take(
            usize::try_from(query.take.unwrap_or(DEFAULT_TAKE).min(DEFAULT_TAKE))
                .unwrap_or(usize::MAX),
        )
        .map(|(_, capsule)| capsule)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_patterns_escape_wildcards() {
        assert_eq!(like_pattern("50%_\\x", true, true), "%50\\%\\_\\\\x%");
        assert_eq!(like_pattern("ab", false, true), "ab%");
    }

    #[test]
    fn filters_compile_to_parameterized_sql() {
        let mapping = table_mapping(&CapsuleKind("core.user".to_owned())).unwrap();
        let mut params = Vec::new();
        let sql = filter_sql(
            mapping,
            Some(&Where::Or(vec![
                Where::field(
                    "username",
                    Cond::Contains("a'; DROP".into(), Case::Insensitive),
                ),
                Where::field("email", Cond::In(vec![Value::from("x"), Value::Null])),
                Where::Not(Box::new(Where::field(
                    "status",
                    Cond::Gte(Value::from("b")),
                ))),
            ])),
            &mut params,
        )
        .unwrap();
        assert!(!sql.contains("DROP"));
        assert!(sql.contains("lower(\"username\") LIKE lower($1)"));
        assert!(sql.contains("\"email\" IN ($2)"));
        assert!(sql.contains("NOT (\"status\" COLLATE \"C\" >= $3)"));
        assert_eq!(params.len(), 3);
        assert!(filter_sql(mapping, Some(&Where::eq("nope", 1)), &mut params).is_err());
    }
}
