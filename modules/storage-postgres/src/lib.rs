//! Native PostgreSQL persistence for core capsule kinds.
#![forbid(unsafe_code)]

use aseman_contracts::capsule::{
    CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery, CapsuleValue, ComparisonOperator,
    MAX_QUERY_DEPTH, MAX_QUERY_LIMIT, OwnerScope, ProviderCapabilities, QueryError, QueryErrorCode,
    QueryPredicate, StorageCapability, StorageClass,
};
use postgres::types::ToSql;
use postgres::{Client, NoTls};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};
use thiserror::Error;
use uuid::Uuid;

pub mod service;
pub use service::PostgresStorageService;
pub mod guest;

const MAPPING_JSON: &str = include_str!("../../../contracts/storage/postgres/core-mapping.json");
#[cfg(test)]
const STORAGE_CLASS_MAPPING_JSON: &str =
    include_str!("../../../contracts/storage/postgres/storage-class-mapping.json");
pub const CORE_MIGRATION: &str = include_str!("../migrations/0001_core.sql");
pub const STORAGE_CLASS_MIGRATION: &str = include_str!("../migrations/0002_storage_classes.sql");
const SCHEMA: &str = "aseman_core";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PostgresProviderConfig {
    #[serde(default)]
    pub apply_migrations: bool,
}

#[derive(Debug, Error)]
pub enum PostgresStorageError {
    #[error("invalid capsule or query: {0}")]
    Invalid(String),
    #[error("capsule revision conflict")]
    Conflict,
    #[error("unsupported storage capability: {0}")]
    Unsupported(String),
    #[error("PostgreSQL is unavailable: {0}")]
    Unavailable(String),
}

pub type StorageResult<T> = Result<T, PostgresStorageError>;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MappingCatalog {
    schema_version: u32,
    schema: String,
    tables: Vec<TableMapping>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TableMapping {
    kind: String,
    table: String,
    fields: BTreeMap<String, String>,
    field_columns: BTreeMap<String, String>,
    required_fields: BTreeSet<String>,
    relationships: BTreeMap<String, RelationshipMapping>,
    unique_indexes: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelationshipMapping {
    target_kind: String,
    target_table: String,
    required: bool,
    on_delete: String,
}

static MAPPING: OnceLock<Result<MappingCatalog, String>> = OnceLock::new();

fn mapping_catalog() -> StorageResult<&'static MappingCatalog> {
    match MAPPING.get_or_init(|| {
        let catalog: MappingCatalog = serde_json::from_str(MAPPING_JSON)
            .map_err(|error| format!("PostgreSQL mapping: {error}"))?;
        validate_mapping(&catalog)?;
        Ok(catalog)
    }) {
        Ok(catalog) => Ok(catalog),
        Err(error) => Err(PostgresStorageError::Invalid(error.clone())),
    }
}

fn validate_mapping(catalog: &MappingCatalog) -> Result<(), String> {
    if catalog.schema_version != 1 || catalog.schema != SCHEMA || catalog.tables.is_empty() {
        return Err("unsupported or empty PostgreSQL mapping".to_owned());
    }
    let tables = catalog
        .tables
        .iter()
        .map(|mapping| mapping.table.as_str())
        .collect::<BTreeSet<_>>();
    if tables.len() != catalog.tables.len() {
        return Err("PostgreSQL table names are not unique".to_owned());
    }
    for mapping in &catalog.tables {
        for identifier in std::iter::once(&mapping.table)
            .chain(mapping.field_columns.values())
            .chain(mapping.relationships.keys())
        {
            if !safe_identifier(identifier) {
                return Err(format!("unsafe PostgreSQL identifier: {identifier}"));
            }
        }
        if !mapping
            .required_fields
            .is_subset(&mapping.fields.keys().cloned().collect())
        {
            return Err(format!("undeclared required field in {}", mapping.kind));
        }
        if mapping.field_columns.keys().collect::<BTreeSet<_>>()
            != mapping.fields.keys().collect::<BTreeSet<_>>()
        {
            return Err(format!(
                "incomplete field column mapping in {}",
                mapping.kind
            ));
        }
        let physical = mapping
            .field_columns
            .values()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let envelope_columns = BTreeSet::from([
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
            "capsule_cbor",
        ]);
        if physical.len() != mapping.field_columns.len()
            || !physical.is_disjoint(&envelope_columns)
            || physical
                .iter()
                .any(|column| mapping.relationships.contains_key(*column))
        {
            return Err(format!("physical column collision in {}", mapping.kind));
        }
        for fields in &mapping.unique_indexes {
            if fields.is_empty()
                || fields.iter().any(|field| {
                    !mapping.fields.contains_key(field)
                        && !mapping.relationships.contains_key(field)
                })
            {
                return Err(format!("invalid unique index in {}", mapping.kind));
            }
        }
        for relationship in mapping.relationships.values() {
            if !tables.contains(relationship.target_table.as_str())
                || !matches!(relationship.on_delete.as_str(), "restrict" | "cascade")
            {
                return Err(format!("invalid relationship mapping in {}", mapping.kind));
            }
        }
    }
    Ok(())
}

fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_lowercase()
            } else {
                byte.is_ascii_lowercase() || byte == b'_' || byte.is_ascii_digit()
            }
        })
}

pub struct PostgresCapsuleRepository {
    client: Mutex<Client>,
}

impl PostgresCapsuleRepository {
    pub fn connect(connection_uri: &str) -> StorageResult<Self> {
        let client = Client::connect(connection_uri, NoTls)
            .map_err(|error| PostgresStorageError::Unavailable(error.to_string()))?;
        Ok(Self {
            client: Mutex::new(client),
        })
    }

    pub fn from_client(client: Client) -> Self {
        Self {
            client: Mutex::new(client),
        }
    }

    pub fn migrate(&self) -> StorageResult<()> {
        self.with_client(|client| {
            client.batch_execute(CORE_MIGRATION)?;
            client.batch_execute(STORAGE_CLASS_MIGRATION)
        })
    }

    #[must_use]
    pub fn capabilities() -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "postgres-core-v1".to_owned(),
            capabilities: BTreeSet::from([
                StorageCapability::TransactionsSingleCapsule,
                StorageCapability::RelationshipsForeignKeys,
                StorageCapability::QueriesRange,
                StorageCapability::IndexesUnique,
                StorageCapability::ConsistencyLinearizable,
                StorageCapability::ConsistencySnapshot,
                StorageCapability::ConsistencyReadCommitted,
            ]),
            max_query_limit: MAX_QUERY_LIMIT,
            max_transaction_capsules: 1,
        }
    }

    pub fn put(
        &self,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> StorageResult<()> {
        capsule
            .verify()
            .map_err(|error| PostgresStorageError::Invalid(error.to_string()))?;
        if capsule.storage_class != StorageClass::Core {
            return Err(PostgresStorageError::Unsupported(
                "this provider slice accepts core storage only".to_owned(),
            ));
        }
        let mapping = table_mapping(&capsule.kind)?;
        let (columns, values) = capsule_values(mapping, capsule)?;
        let expected_canonical = capsule
            .canonical_bytes()
            .map_err(|error| PostgresStorageError::Invalid(error.to_string()))?;
        let capsule_id = Uuid::from_bytes(capsule.id.0);
        let parameters = sql_parameters(&values);
        let mut client = self
            .client
            .lock()
            .map_err(|_| PostgresStorageError::Unavailable("client lock poisoned".to_owned()))?;

        let changed = match expected_revision {
            None => {
                if capsule.revision != 1 {
                    return Err(PostgresStorageError::Conflict);
                }
                let placeholders = (1..=columns.len())
                    .map(|index| format!("${index}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let statement = format!(
                    "INSERT INTO {SCHEMA}.{} ({}) VALUES ({placeholders}) \
                     ON CONFLICT (id) DO NOTHING RETURNING revision",
                    sql_identifier(&mapping.table),
                    columns
                        .iter()
                        .map(|column| sql_identifier(column))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                client.query_opt(&statement, &parameters)
            }
            Some(expected) => {
                if capsule.revision != expected.saturating_add(1) {
                    return Err(PostgresStorageError::Conflict);
                }
                let expected = i64::try_from(expected)
                    .map_err(|_| PostgresStorageError::Invalid("revision overflow".to_owned()))?;
                let mut update_values = values;
                update_values.push(SqlParam::I64(Some(expected)));
                let parameters = sql_parameters(&update_values);
                let assignments = columns
                    .iter()
                    .enumerate()
                    .skip(1)
                    .map(|(index, column)| format!("{} = ${}", sql_identifier(column), index + 1))
                    .collect::<Vec<_>>()
                    .join(", ");
                let expected_parameter = columns.len() + 1;
                let statement = format!(
                    "UPDATE {SCHEMA}.{} SET {assignments} WHERE id = $1 \
                     AND revision = ${expected_parameter} AND $3 = revision + 1 \
                     AND $4 = created_at_micros AND $6 = integrity_hash RETURNING revision",
                    sql_identifier(&mapping.table)
                );
                client.query_opt(&statement, &parameters)
            }
        }
        .map_err(map_postgres_error)?;
        if changed.is_none() {
            let statement = format!(
                "SELECT capsule_cbor FROM {SCHEMA}.{} WHERE id = $1",
                sql_identifier(&mapping.table)
            );
            let existing = client
                .query_opt(&statement, &[&capsule_id])
                .map_err(map_postgres_error)?
                .map(|row| row.get::<_, Vec<u8>>(0));
            if existing.as_deref() == Some(expected_canonical.as_slice()) {
                return Ok(());
            }
            return Err(PostgresStorageError::Conflict);
        }
        Ok(())
    }

    pub fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> StorageResult<Option<CapsuleEnvelope>> {
        let mapping = table_mapping(kind)?;
        let id = Uuid::from_bytes(id.0);
        let statement = format!(
            "SELECT capsule_cbor FROM {SCHEMA}.{} WHERE id = $1",
            sql_identifier(&mapping.table)
        );
        let row = self.with_client(|client| client.query_opt(&statement, &[&id]))?;
        row.map(|row| {
            let bytes: Vec<u8> = row.get(0);
            CapsuleEnvelope::from_canonical_bytes(&bytes)
                .map_err(|error| PostgresStorageError::Invalid(error.to_string()))
        })
        .transpose()
    }

    pub fn query(&self, query: &CapsuleQuery) -> StorageResult<Vec<CapsuleEnvelope>> {
        if query.limit == 0 || query.limit > MAX_QUERY_LIMIT {
            return Err(PostgresStorageError::Invalid(
                "query limit is outside provider bounds".to_owned(),
            ));
        }
        if !query.aggregates.is_empty() || !query.traversals.is_empty() || query.cursor.is_some() {
            return Err(PostgresStorageError::Unsupported(
                "aggregates, traversal, and cursors are not advertised by postgres-core-v1"
                    .to_owned(),
            ));
        }
        let mapping = table_mapping(&query.kind)?;
        for field in &query.projection {
            require_query_field(mapping, field)?;
        }
        let mut values = Vec::new();
        let mut filters = vec!["NOT tombstone".to_owned()];
        if let Some(predicate) = &query.predicate {
            filters.push(predicate_sql(predicate, mapping, &mut values, 1)?);
        }
        let mut order = Vec::new();
        for sort in &query.sort {
            let column = require_query_field(mapping, &sort.field)?;
            let direction = match sort.direction {
                aseman_contracts::capsule::SortDirection::Ascending => "ASC",
                aseman_contracts::capsule::SortDirection::Descending => "DESC",
            };
            order.push(format!("{} {direction}", sql_identifier(column)));
        }
        order.push("id ASC".to_owned());
        values.push(SqlParam::I64(Some(i64::from(query.limit))));
        let limit_parameter = values.len();
        let statement = format!(
            "SELECT capsule_cbor FROM {SCHEMA}.{} WHERE {} ORDER BY {} LIMIT ${limit_parameter}",
            sql_identifier(&mapping.table),
            filters.join(" AND "),
            order.join(", ")
        );
        let parameters = sql_parameters(&values);
        let rows = self.with_client(|client| client.query(&statement, &parameters))?;
        rows.into_iter()
            .map(|row| {
                let bytes: Vec<u8> = row.get(0);
                CapsuleEnvelope::from_canonical_bytes(&bytes)
                    .map_err(|error| PostgresStorageError::Invalid(error.to_string()))
            })
            .collect()
    }

    fn with_client<T>(
        &self,
        operation: impl FnOnce(&mut Client) -> Result<T, postgres::Error>,
    ) -> StorageResult<T> {
        let mut client = self
            .client
            .lock()
            .map_err(|_| PostgresStorageError::Unavailable("client lock poisoned".to_owned()))?;
        operation(&mut client).map_err(map_postgres_error)
    }
}

fn table_mapping(kind: &CapsuleKind) -> StorageResult<&'static TableMapping> {
    mapping_catalog()?
        .tables
        .iter()
        .find(|mapping| mapping.kind == kind.0)
        .ok_or_else(|| PostgresStorageError::Unsupported(format!("unmapped kind {}", kind.0)))
}

fn capsule_values(
    mapping: &TableMapping,
    capsule: &CapsuleEnvelope,
) -> StorageResult<(Vec<String>, Vec<SqlParam>)> {
    let revision = i64::try_from(capsule.revision)
        .map_err(|_| PostgresStorageError::Invalid("revision overflow".to_owned()))?;
    let schema_version = i32::try_from(capsule.schema_version)
        .map_err(|_| PostgresStorageError::Invalid("schema version overflow".to_owned()))?;
    let (owner_type, owner_id, owner_name) = match &capsule.owner_scope {
        OwnerScope::Global => ("global", None, None),
        OwnerScope::Node(id) => ("node", Some(Uuid::from_bytes(*id)), None),
        OwnerScope::Creature(id) => ("creature", Some(Uuid::from_bytes(*id)), None),
        OwnerScope::Module(name) => ("module", None, Some(name.clone())),
    };
    let canonical = capsule
        .canonical_bytes()
        .map_err(|error| PostgresStorageError::Invalid(error.to_string()))?;
    let mut columns = vec![
        "id".to_owned(),
        "schema_version".to_owned(),
        "revision".to_owned(),
        "created_at_micros".to_owned(),
        "updated_at_micros".to_owned(),
        "previous_integrity".to_owned(),
        "integrity_hash".to_owned(),
        "owner_type".to_owned(),
        "owner_id".to_owned(),
        "owner_name".to_owned(),
        "tombstone".to_owned(),
        "capsule_cbor".to_owned(),
    ];
    let mut values = vec![
        SqlParam::Uuid(Some(Uuid::from_bytes(capsule.id.0))),
        SqlParam::I32(Some(schema_version)),
        SqlParam::I64(Some(revision)),
        SqlParam::I64(Some(capsule.created_at_micros)),
        SqlParam::I64(Some(capsule.updated_at_micros)),
        SqlParam::Bytes(
            capsule
                .previous_integrity
                .as_ref()
                .map(|value| value.bytes.clone()),
        ),
        SqlParam::Bytes(Some(capsule.integrity_hash.bytes.clone())),
        SqlParam::Text(Some(owner_type.to_owned())),
        SqlParam::Uuid(owner_id),
        SqlParam::Text(owner_name),
        SqlParam::Bool(Some(capsule.tombstone)),
        SqlParam::Bytes(Some(canonical)),
    ];
    let body = match &capsule.body {
        Some(CapsuleValue::Object(body)) => Some(body),
        Some(_) => {
            return Err(PostgresStorageError::Invalid(
                "core capsule body must be an object".to_owned(),
            ));
        }
        None => None,
    };
    if let Some(body) = body {
        if body.keys().any(|field| !mapping.fields.contains_key(field)) {
            return Err(PostgresStorageError::Invalid(
                "capsule body contains an undeclared field".to_owned(),
            ));
        }
        if mapping
            .required_fields
            .iter()
            .any(|field| !body.contains_key(field) || matches!(body[field], CapsuleValue::Null))
        {
            return Err(PostgresStorageError::Invalid(
                "capsule body omits a required field".to_owned(),
            ));
        }
    }
    for (name, field_type) in &mapping.fields {
        columns.push(mapping.field_columns[name].clone());
        let value = body.and_then(|body| body.get(name));
        values.push(field_value(field_type, value)?);
    }

    let relationships = capsule
        .relationships
        .iter()
        .map(|relationship| (relationship.name.as_str(), relationship))
        .collect::<BTreeMap<_, _>>();
    if relationships
        .keys()
        .any(|name| !mapping.relationships.contains_key(*name))
    {
        return Err(PostgresStorageError::Invalid(
            "capsule contains an undeclared relationship".to_owned(),
        ));
    }
    for (name, relationship_mapping) in &mapping.relationships {
        columns.push(name.clone());
        let relationship = relationships.get(name.as_str());
        if relationship_mapping.required && relationship.is_none() {
            return Err(PostgresStorageError::Invalid(format!(
                "required relationship {name} is absent"
            )));
        }
        if let Some(relationship) = relationship
            && relationship.target_kind.0 != relationship_mapping.target_kind
        {
            return Err(PostgresStorageError::Invalid(format!(
                "relationship {name} has the wrong target kind"
            )));
        }
        values.push(SqlParam::Uuid(
            relationship.map(|value| Uuid::from_bytes(value.target_id.0)),
        ));
    }
    Ok((columns, values))
}

fn field_value(field_type: &str, value: Option<&CapsuleValue>) -> StorageResult<SqlParam> {
    match (field_type, value) {
        (_, None | Some(CapsuleValue::Null)) => Ok(match field_type {
            "bool" => SqlParam::Bool(None),
            "integer" | "timestamp_micros" => SqlParam::I64(None),
            "float" => SqlParam::F64(None),
            "bytes" => SqlParam::Bytes(None),
            "text" => SqlParam::Text(None),
            "capsule_id" => SqlParam::Uuid(None),
            _ => {
                return Err(PostgresStorageError::Invalid(
                    "unknown field type".to_owned(),
                ));
            }
        }),
        ("bool", Some(CapsuleValue::Bool(value))) => Ok(SqlParam::Bool(Some(*value))),
        ("integer" | "timestamp_micros", Some(CapsuleValue::Integer(value))) => {
            Ok(SqlParam::I64(Some(*value)))
        }
        ("float", Some(CapsuleValue::Float(value))) => Ok(SqlParam::F64(Some(*value))),
        ("float", Some(CapsuleValue::Integer(value))) => {
            let converted = *value as f64;
            if converted as i64 != *value {
                return Err(PostgresStorageError::Invalid(
                    "integer cannot be represented exactly as a PostgreSQL float".to_owned(),
                ));
            }
            Ok(SqlParam::F64(Some(converted)))
        }
        ("bytes", Some(CapsuleValue::Bytes(value))) => Ok(SqlParam::Bytes(Some(value.clone()))),
        ("text", Some(CapsuleValue::Text(value))) => Ok(SqlParam::Text(Some(value.clone()))),
        ("capsule_id", Some(CapsuleValue::Bytes(value))) if value.len() == 16 => {
            let bytes: [u8; 16] = value
                .as_slice()
                .try_into()
                .map_err(|_| PostgresStorageError::Invalid("capsule ID length".to_owned()))?;
            Ok(SqlParam::Uuid(Some(Uuid::from_bytes(bytes))))
        }
        _ => Err(PostgresStorageError::Invalid(format!(
            "capsule value does not match field type {field_type}"
        ))),
    }
}

fn predicate_sql(
    predicate: &QueryPredicate,
    mapping: &TableMapping,
    values: &mut Vec<SqlParam>,
    depth: usize,
) -> StorageResult<String> {
    if depth > MAX_QUERY_DEPTH {
        return Err(PostgresStorageError::Invalid(
            "query predicate nesting exceeds the limit".to_owned(),
        ));
    }
    match predicate {
        QueryPredicate::Compare {
            field,
            operator,
            value,
        } => {
            let field_type = mapping.fields.get(field).ok_or_else(|| {
                PostgresStorageError::Invalid("query field is not declared".to_owned())
            })?;
            let column = &mapping.field_columns[field];
            if matches!(value, CapsuleValue::Null) {
                return match operator {
                    ComparisonOperator::Equal => Ok(format!("{} IS NULL", sql_identifier(column))),
                    ComparisonOperator::NotEqual => {
                        Ok(format!("{} IS NOT NULL", sql_identifier(column)))
                    }
                    _ => Err(PostgresStorageError::Invalid(
                        "null supports only equality comparisons".to_owned(),
                    )),
                };
            }
            values.push(field_value(field_type, Some(value))?);
            let parameter = values.len();
            let operator = match operator {
                ComparisonOperator::Equal => "=",
                ComparisonOperator::NotEqual => "<>",
                ComparisonOperator::LessThan => "<",
                ComparisonOperator::LessOrEqual => "<=",
                ComparisonOperator::GreaterThan => ">",
                ComparisonOperator::GreaterOrEqual => ">=",
            };
            Ok(format!(
                "{} {operator} ${parameter}",
                sql_identifier(column)
            ))
        }
        QueryPredicate::And { predicates } | QueryPredicate::Or { predicates } => {
            if predicates.is_empty() || predicates.len() > 64 {
                return Err(PostgresStorageError::Invalid(
                    "boolean predicate width is invalid".to_owned(),
                ));
            }
            let join = if matches!(predicate, QueryPredicate::And { .. }) {
                " AND "
            } else {
                " OR "
            };
            let children = predicates
                .iter()
                .map(|child| predicate_sql(child, mapping, values, depth + 1))
                .collect::<StorageResult<Vec<_>>>()?;
            Ok(format!("({})", children.join(join)))
        }
        QueryPredicate::Not { predicate } => Ok(format!(
            "NOT ({})",
            predicate_sql(predicate, mapping, values, depth + 1)?
        )),
        QueryPredicate::RelationshipExists { relationship } => {
            if mapping.relationships.contains_key(relationship) {
                Ok(format!("{} IS NOT NULL", sql_identifier(relationship)))
            } else {
                Err(PostgresStorageError::Invalid(
                    "query relationship is not declared".to_owned(),
                ))
            }
        }
    }
}

fn require_query_field<'a>(mapping: &'a TableMapping, field: &str) -> StorageResult<&'a str> {
    mapping
        .field_columns
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| PostgresStorageError::Invalid("query field is not declared".to_owned()))
}

fn sql_identifier(value: &str) -> String {
    debug_assert!(safe_identifier(value));
    format!("\"{value}\"")
}

enum SqlParam {
    Bool(Option<bool>),
    I32(Option<i32>),
    I64(Option<i64>),
    F64(Option<f64>),
    Bytes(Option<Vec<u8>>),
    Text(Option<String>),
    Uuid(Option<Uuid>),
}

impl SqlParam {
    fn as_sql(&self) -> &(dyn ToSql + Sync) {
        match self {
            Self::Bool(value) => value,
            Self::I32(value) => value,
            Self::I64(value) => value,
            Self::F64(value) => value,
            Self::Bytes(value) => value,
            Self::Text(value) => value,
            Self::Uuid(value) => value,
        }
    }
}

fn sql_parameters(values: &[SqlParam]) -> Vec<&(dyn ToSql + Sync)> {
    values.iter().map(SqlParam::as_sql).collect()
}

fn map_postgres_error(error: postgres::Error) -> PostgresStorageError {
    if let Some(database_error) = error.as_db_error() {
        match database_error.code().code() {
            "23505" => return PostgresStorageError::Conflict,
            "23502" | "23503" | "23514" | "22P02" => {
                return PostgresStorageError::Invalid(database_error.message().to_owned());
            }
            code => {
                return PostgresStorageError::Unavailable(format!(
                    "{} (SQLSTATE {code})",
                    database_error.message()
                ));
            }
        }
    }
    PostgresStorageError::Unavailable(error.to_string())
}

impl From<PostgresStorageError> for QueryError {
    fn from(error: PostgresStorageError) -> Self {
        let (code, retryable) = match error {
            PostgresStorageError::Invalid(_) => (QueryErrorCode::InvalidQuery, false),
            PostgresStorageError::Conflict => (QueryErrorCode::RevisionConflict, false),
            PostgresStorageError::Unsupported(_) => (QueryErrorCode::UnsupportedCapability, false),
            PostgresStorageError::Unavailable(_) => (QueryErrorCode::Unavailable, true),
        };
        QueryError {
            message: error.to_string(),
            code,
            retryable,
            missing_capabilities: BTreeSet::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_mapping_is_closed_safe_and_complete() {
        let catalog = mapping_catalog().unwrap();
        assert_eq!(catalog.tables.len(), 20);
        assert!(
            catalog
                .tables
                .iter()
                .all(|table| table.table != "guest_capsules")
        );
        assert!(
            catalog
                .tables
                .iter()
                .any(|table| table.table == "guest_database_bindings")
        );
        assert!(CORE_MIGRATION.contains("REVOKE ALL ON SCHEMA aseman_core FROM PUBLIC"));
        assert!(!CORE_MIGRATION.to_ascii_lowercase().contains("jsonb"));
    }

    #[test]
    fn generated_storage_class_mapping_is_native_and_policy_specific() {
        let mapping: serde_json::Value = serde_json::from_str(STORAGE_CLASS_MAPPING_JSON).unwrap();
        let tables = mapping["tables"].as_array().unwrap();
        assert_eq!(tables.len(), 13);
        assert_eq!(
            tables
                .iter()
                .filter(|row| row["mutation_policy"] == "append_only")
                .count(),
            5
        );
        assert!(tables.iter().all(|row| row["schema"] != "aseman_core"));
        assert!(STORAGE_CLASS_MIGRATION.contains("USING BRIN"));
        assert!(STORAGE_CLASS_MIGRATION.contains("reject_capsule_mutation"));
        assert!(
            !STORAGE_CLASS_MIGRATION
                .to_ascii_lowercase()
                .contains("jsonb")
        );
        assert!(!STORAGE_CLASS_MIGRATION.contains("guest_capsules"));
    }

    #[test]
    fn query_translation_is_parameterized_and_rejects_unknown_fields() {
        let mapping = table_mapping(&CapsuleKind("core.user".to_owned())).unwrap();
        let predicate = QueryPredicate::Compare {
            field: "username".to_owned(),
            operator: ComparisonOperator::Equal,
            value: CapsuleValue::Text("' OR TRUE --".to_owned()),
        };
        let mut values = Vec::new();
        assert_eq!(
            predicate_sql(&predicate, mapping, &mut values, 1).unwrap(),
            "\"username\" = $1"
        );
        let mut unknown = values;
        assert!(
            predicate_sql(
                &QueryPredicate::Compare {
                    field: "not_a_column".to_owned(),
                    operator: ComparisonOperator::Equal,
                    value: CapsuleValue::Text("x".to_owned()),
                },
                mapping,
                &mut unknown,
                1,
            )
            .is_err()
        );
    }
}
