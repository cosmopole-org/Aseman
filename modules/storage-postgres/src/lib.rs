//! Native PostgreSQL persistence for core capsule kinds.
#![forbid(unsafe_code)]

use aseman_contracts::capsule::{
    CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery, CapsuleValue, ComparisonOperator,
    MAX_QUERY_DEPTH, MAX_QUERY_LIMIT, OwnerScope, ProviderCapabilities, QueryError, QueryErrorCode,
    QueryPredicate, StorageCapability, StorageClass,
};
use postgres::types::ToSql;
use postgres::{Client, GenericClient, NoTls};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};
use thiserror::Error;
use uuid::Uuid;

pub mod service;
pub use service::PostgresStorageService;
pub mod capsule_store;
pub mod guest;
pub mod migration;
mod replay;
pub mod unit_of_work;
pub mod vmm;

const MAPPING_JSON: &str = include_str!("../../../contracts/storage/postgres/core-mapping.json");
#[cfg(test)]
const STORAGE_CLASS_MAPPING_JSON: &str =
    include_str!("../../../contracts/storage/postgres/storage-class-mapping.json");
pub const RETIRE_MIGRATION: &str = include_str!("../migrations/0000_retire_obsolete.sql");
pub const CORE_MIGRATION: &str = include_str!("../migrations/0001_core.sql");
pub const STORAGE_CLASS_MIGRATION: &str = include_str!("../migrations/0002_storage_classes.sql");
pub const MIGRATION_FENCE_MIGRATION: &str = include_str!("../migrations/0003_migration_fence.sql");
pub const PROGRAM_MACHINE_MIGRATION: &str =
    include_str!("../migrations/0004_program_machine_not_unique.sql");
pub const IDENTITY_KEYS_MIGRATION: &str = include_str!("../migrations/0005_identity_keys.sql");
pub(crate) const SCHEMA: &str = "aseman_core";
/// The most capsules one [`PostgresCapsuleRepository::put_all`] transaction holds.
pub const MAX_TRANSACTION_CAPSULES: usize = 64;

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
    /// PostgreSQL schema; core rows omit it (`aseman_core`).
    #[serde(default = "core_schema")]
    schema: String,
    /// Capsule storage class the table accepts.
    #[serde(default = "core_class")]
    storage_class: String,
    /// Append-only kinds (A307) never accept a revision update.
    #[serde(default)]
    append_only: bool,
    table: String,
    fields: BTreeMap<String, String>,
    field_columns: BTreeMap<String, String>,
    /// Schemaless document fields (ADR 0016). They carry a structured capsule
    /// object in the canonical envelope and never receive a native column.
    document_fields: BTreeSet<String>,
    required_fields: BTreeSet<String>,
    relationships: BTreeMap<String, RelationshipMapping>,
    unique_indexes: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelationshipMapping {
    target_kind: String,
    #[serde(default = "core_schema")]
    target_schema: String,
    target_table: String,
    required: bool,
    on_delete: String,
}

fn core_schema() -> String {
    SCHEMA.to_owned()
}

fn core_class() -> String {
    "core".to_owned()
}

/// One row of the generated storage-class mapping (A307).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassTableMapping {
    kind: String,
    schema: String,
    table: String,
    storage_class: String,
    consistency: String,
    required_capabilities: Vec<String>,
    fields: BTreeMap<String, String>,
    field_columns: BTreeMap<String, String>,
    document_fields: BTreeSet<String>,
    required_fields: BTreeSet<String>,
    relationships: BTreeMap<String, RelationshipMapping>,
    unique_indexes: Vec<Vec<String>>,
    range_indexes: Vec<Vec<String>>,
    retention: String,
    mutation_policy: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassMappingCatalog {
    schema_version: u32,
    schemas: Vec<String>,
    tables: Vec<ClassTableMapping>,
}

const CLASS_MAPPING_JSON: &str =
    include_str!("../../../contracts/storage/postgres/storage-class-mapping.json");

static ALL_TABLES: OnceLock<Result<Vec<TableMapping>, String>> = OnceLock::new();

/// Every mapped table: the core catalog plus the storage-class catalog.
fn all_tables() -> StorageResult<&'static [TableMapping]> {
    match ALL_TABLES.get_or_init(|| {
        let mut tables = mapping_catalog()
            .map_err(|error| error.to_string())?
            .tables
            .clone();
        let classes: ClassMappingCatalog = serde_json::from_str(CLASS_MAPPING_JSON)
            .map_err(|error| format!("PostgreSQL storage-class mapping: {error}"))?;
        if classes.schema_version != 1 || classes.schemas.is_empty() {
            return Err("unsupported storage-class mapping".to_owned());
        }
        for row in classes.tables {
            // Consistency, capabilities, range indexes, and retention are enforced by the
            // generated DDL and activation negotiation; the writer needs only the fields below.
            let _ = (
                &row.consistency,
                &row.required_capabilities,
                &row.range_indexes,
                &row.retention,
            );
            tables.push(TableMapping {
                kind: row.kind,
                schema: row.schema,
                storage_class: row.storage_class,
                append_only: row.mutation_policy == "append_only",
                table: row.table,
                fields: row.fields,
                field_columns: row.field_columns,
                document_fields: row.document_fields,
                required_fields: row.required_fields,
                relationships: row.relationships,
                unique_indexes: row.unique_indexes,
            });
        }
        let physical = tables
            .iter()
            .map(|mapping| (mapping.schema.as_str(), mapping.table.as_str()))
            .collect::<BTreeSet<_>>();
        for mapping in &tables {
            if !safe_identifier(&mapping.schema) || !safe_identifier(&mapping.table) {
                return Err(format!("unsafe identifier in {}", mapping.kind));
            }
            for relationship in mapping.relationships.values() {
                if !physical.contains(&(
                    relationship.target_schema.as_str(),
                    relationship.target_table.as_str(),
                )) {
                    return Err(format!("relationship target missing in {}", mapping.kind));
                }
            }
        }
        if physical.len() != tables.len() {
            return Err("mapped tables are not unique".to_owned());
        }
        Ok(tables)
    }) {
        Ok(tables) => Ok(tables),
        Err(error) => Err(PostgresStorageError::Invalid(error.clone())),
    }
}

fn storage_class_name(class: &StorageClass) -> &str {
    match class {
        StorageClass::Core => "core",
        StorageClass::GuestData => "guest_data",
        StorageClass::Telemetry => "telemetry",
        StorageClass::Audit => "audit",
        StorageClass::Finance => "finance",
        StorageClass::Outbox => "outbox",
        StorageClass::Realtime => "realtime",
        StorageClass::Module(_) => "module",
    }
}

fn qualified(mapping: &TableMapping) -> String {
    format!(
        "{}.{}",
        sql_identifier(&mapping.schema),
        sql_identifier(&mapping.table)
    )
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
        let declared_fields = mapping
            .fields
            .keys()
            .chain(mapping.document_fields.iter())
            .cloned()
            .collect::<BTreeSet<_>>();
        if !mapping.required_fields.is_subset(&declared_fields) {
            return Err(format!("undeclared required field in {}", mapping.kind));
        }
        if mapping.document_fields.iter().any(|field| {
            mapping.fields.contains_key(field) || mapping.relationships.contains_key(field)
        }) {
            return Err(format!("document field is also mapped in {}", mapping.kind));
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
                || fields
                    .iter()
                    .any(|field| mapping.document_fields.contains(field))
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
            client.batch_execute(RETIRE_MIGRATION)?;
            client.batch_execute(CORE_MIGRATION)?;
            client.batch_execute(STORAGE_CLASS_MIGRATION)?;
            client.batch_execute(MIGRATION_FENCE_MIGRATION)?;
            client.batch_execute(PROGRAM_MACHINE_MIGRATION)?;
            client.batch_execute(IDENTITY_KEYS_MIGRATION)
        })
    }

    #[must_use]
    pub fn capabilities() -> ProviderCapabilities {
        ProviderCapabilities {
            provider_id: "postgres-core-v1".to_owned(),
            capabilities: BTreeSet::from([
                StorageCapability::TransactionsSingleCapsule,
                StorageCapability::TransactionsMultiCapsule,
                StorageCapability::RelationshipsForeignKeys,
                StorageCapability::QueriesRange,
                StorageCapability::IndexesUnique,
                StorageCapability::ConsistencyLinearizable,
                StorageCapability::ConsistencySnapshot,
                StorageCapability::ConsistencyReadCommitted,
            ]),
            max_query_limit: MAX_QUERY_LIMIT,
            max_transaction_capsules: MAX_TRANSACTION_CAPSULES as u32,
        }
    }

    pub fn put(
        &self,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> StorageResult<()> {
        self.put_fenced(capsule, expected_revision, None)
    }

    /// Put under a binding generation: inside the same transaction, a write whose
    /// generation is below `aseman_core.migration_fence.min_generation` is refused
    /// with `Conflict` (A309 fencing).
    pub fn put_fenced(
        &self,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
        generation: Option<u64>,
    ) -> StorageResult<()> {
        self.put_all_fenced(&[(capsule.clone(), expected_revision)], generation)
    }

    /// Put several capsules in one transaction: every write applies, or none does.
    pub fn put_all(&self, writes: &[(CapsuleEnvelope, Option<u64>)]) -> StorageResult<()> {
        self.put_all_fenced(writes, None)
    }

    /// [`Self::put_all`] under a binding generation, checked once for the whole
    /// transaction (A309 fencing).
    pub fn put_all_fenced(
        &self,
        writes: &[(CapsuleEnvelope, Option<u64>)],
        generation: Option<u64>,
    ) -> StorageResult<()> {
        if writes.len() > MAX_TRANSACTION_CAPSULES {
            return Err(PostgresStorageError::Unsupported(format!(
                "a transaction holds at most {MAX_TRANSACTION_CAPSULES} capsules"
            )));
        }
        let mut prepared = Vec::with_capacity(writes.len());
        for (capsule, expected_revision) in writes {
            prepared.push(prepare_write(capsule, *expected_revision)?);
        }
        let mut guard = self
            .client
            .lock()
            .map_err(|_| PostgresStorageError::Unavailable("client lock poisoned".to_owned()))?;
        let mut client = guard.transaction().map_err(map_postgres_error)?;
        if let Some(generation) = generation {
            let minimum: i64 = client
                .query_one(
                    &format!("SELECT min_generation FROM {SCHEMA}.migration_fence FOR SHARE"),
                    &[],
                )
                .map_err(map_postgres_error)?
                .get(0);
            if i64::try_from(generation).map_or(true, |generation| generation < minimum) {
                return Err(PostgresStorageError::Conflict);
            }
        }
        for write in &prepared {
            write_prepared(&mut client, write)?;
        }
        client.commit().map_err(map_postgres_error)
    }

    /// Raise the fenced minimum generation; it never decreases.
    pub fn raise_fence(&self, generation: u64) -> StorageResult<()> {
        let generation = i64::try_from(generation)
            .map_err(|_| PostgresStorageError::Invalid("generation overflow".to_owned()))?;
        self.with_client(|client| {
            client
                .execute(
                    &format!(
                        "UPDATE {SCHEMA}.migration_fence SET min_generation = GREATEST(min_generation, $1)"
                    ),
                    &[&generation],
                )
                .map(|_| ())
        })
    }

    /// Every capsule in every mapped table (core and storage classes), in
    /// deterministic table/ID order, for A309 comparison.
    pub fn snapshot_all(&self) -> StorageResult<Vec<CapsuleEnvelope>> {
        let tables = all_tables()?.iter().map(qualified).collect::<Vec<_>>();
        let rows = self.with_client(|client| {
            let mut rows = Vec::new();
            for table in &tables {
                let statement = format!("SELECT capsule_cbor FROM {table} ORDER BY id");
                for row in client.query(&statement, &[])? {
                    rows.push(row.get::<_, Vec<u8>>(0));
                }
            }
            Ok(rows)
        })?;
        rows.iter()
            .map(|bytes| {
                CapsuleEnvelope::from_canonical_bytes(bytes)
                    .map_err(|error| PostgresStorageError::Invalid(error.to_string()))
            })
            .collect()
    }

    pub fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> StorageResult<Option<CapsuleEnvelope>> {
        let mut guard = self
            .client
            .lock()
            .map_err(|_| PostgresStorageError::Unavailable("client lock poisoned".to_owned()))?;
        get_on(&mut *guard, kind, id)
    }

    pub fn query(&self, query: &CapsuleQuery) -> StorageResult<Vec<CapsuleEnvelope>> {
        let mut guard = self
            .client
            .lock()
            .map_err(|_| PostgresStorageError::Unavailable("client lock poisoned".to_owned()))?;
        query_on(&mut *guard, query)
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

/// Read one capsule on `client` (a connection or an open transaction).
pub(crate) fn get_on(
    client: &mut impl GenericClient,
    kind: &CapsuleKind,
    id: &CapsuleId,
) -> StorageResult<Option<CapsuleEnvelope>> {
    let mapping = table_mapping(kind)?;
    let id = Uuid::from_bytes(id.0);
    let statement = format!(
        "SELECT capsule_cbor FROM {} WHERE id = $1",
        qualified(mapping)
    );
    let row = client
        .query_opt(&statement, &[&id])
        .map_err(map_postgres_error)?;
    row.map(|row| {
        let bytes: Vec<u8> = row.get(0);
        CapsuleEnvelope::from_canonical_bytes(&bytes)
            .map_err(|error| PostgresStorageError::Invalid(error.to_string()))
    })
    .transpose()
}

/// Run a capsule query on `client` (a connection or an open transaction).
pub(crate) fn query_on(
    client: &mut impl GenericClient,
    query: &CapsuleQuery,
) -> StorageResult<Vec<CapsuleEnvelope>> {
    if query.limit == 0 || query.limit > MAX_QUERY_LIMIT {
        return Err(PostgresStorageError::Invalid(
            "query limit is outside provider bounds".to_owned(),
        ));
    }
    if !query.aggregates.is_empty() || !query.traversals.is_empty() || query.cursor.is_some() {
        return Err(PostgresStorageError::Unsupported(
            "aggregates, traversal, and cursors are not advertised by postgres-core-v1".to_owned(),
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
        "SELECT capsule_cbor FROM {} WHERE {} ORDER BY {} LIMIT ${limit_parameter}",
        qualified(mapping),
        filters.join(" AND "),
        order.join(", ")
    );
    let parameters = sql_parameters(&values);
    let rows = client
        .query(&statement, &parameters)
        .map_err(map_postgres_error)?;
    rows.into_iter()
        .map(|row| {
            let bytes: Vec<u8> = row.get(0);
            CapsuleEnvelope::from_canonical_bytes(&bytes)
                .map_err(|error| PostgresStorageError::Invalid(error.to_string()))
        })
        .collect()
}

/// A validated write, ready to run inside a transaction.
pub(crate) struct PreparedWrite<'a> {
    mapping: &'static TableMapping,
    capsule: &'a CapsuleEnvelope,
    expected_revision: Option<u64>,
    columns: Vec<String>,
    values: Vec<SqlParam>,
    canonical: Vec<u8>,
}

pub(crate) fn prepare_write(
    capsule: &CapsuleEnvelope,
    expected_revision: Option<u64>,
) -> StorageResult<PreparedWrite<'_>> {
    capsule
        .verify()
        .map_err(|error| PostgresStorageError::Invalid(error.to_string()))?;
    let mapping = table_mapping(&capsule.kind)?;
    if storage_class_name(&capsule.storage_class) != mapping.storage_class {
        return Err(PostgresStorageError::Invalid(format!(
            "{} must use the {} storage class",
            capsule.kind.0, mapping.storage_class
        )));
    }
    if mapping.append_only && (expected_revision.is_some() || capsule.revision != 1) {
        return Err(PostgresStorageError::Unsupported(format!(
            "{} is append-only",
            capsule.kind.0
        )));
    }
    let (columns, values) = capsule_values(mapping, capsule)?;
    let canonical = capsule
        .canonical_bytes()
        .map_err(|error| PostgresStorageError::Invalid(error.to_string()))?;
    Ok(PreparedWrite {
        mapping,
        capsule,
        expected_revision,
        columns,
        values,
        canonical,
    })
}

/// Insert or compare-and-swap one capsule inside `client`'s transaction. An identical
/// replay of a stored revision succeeds; any other lost race is `Conflict`.
pub(crate) fn write_prepared(
    client: &mut impl GenericClient,
    write: &PreparedWrite<'_>,
) -> StorageResult<()> {
    let PreparedWrite {
        mapping,
        capsule,
        expected_revision,
        columns,
        values,
        canonical,
    } = write;
    let capsule_id = Uuid::from_bytes(capsule.id.0);
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
                "INSERT INTO {} ({}) VALUES ({placeholders}) \
                 ON CONFLICT (id) DO NOTHING RETURNING revision",
                qualified(mapping),
                columns
                    .iter()
                    .map(|column| sql_identifier(column))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            client.query_opt(&statement, &sql_parameters(values))
        }
        Some(expected) => {
            if capsule.revision != expected.saturating_add(1) {
                return Err(PostgresStorageError::Conflict);
            }
            let expected = i64::try_from(*expected)
                .map_err(|_| PostgresStorageError::Invalid("revision overflow".to_owned()))?;
            let mut update_values = values.clone();
            update_values.push(SqlParam::I64(Some(expected)));
            let assignments = columns
                .iter()
                .enumerate()
                .skip(1)
                .map(|(index, column)| format!("{} = ${}", sql_identifier(column), index + 1))
                .collect::<Vec<_>>()
                .join(", ");
            let expected_parameter = columns.len() + 1;
            let statement = format!(
                "UPDATE {} SET {assignments} WHERE id = $1 \
                 AND revision = ${expected_parameter} AND $3 = revision + 1 \
                 AND $4 = created_at_micros AND $6 = integrity_hash RETURNING revision",
                qualified(mapping)
            );
            client.query_opt(&statement, &sql_parameters(&update_values))
        }
    }
    .map_err(map_postgres_error)?;
    if changed.is_some() {
        return Ok(());
    }
    let statement = format!(
        "SELECT capsule_cbor FROM {} WHERE id = $1",
        qualified(mapping)
    );
    let existing = client
        .query_opt(&statement, &[&capsule_id])
        .map_err(map_postgres_error)?
        .map(|row| row.get::<_, Vec<u8>>(0));
    if existing.as_deref() == Some(canonical.as_slice()) {
        return Ok(());
    }
    Err(PostgresStorageError::Conflict)
}

fn table_mapping(kind: &CapsuleKind) -> StorageResult<&'static TableMapping> {
    all_tables()?
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
        if body.keys().any(|field| {
            !mapping.fields.contains_key(field) && !mapping.document_fields.contains(field)
        }) {
            return Err(PostgresStorageError::Invalid(
                "capsule body contains an undeclared field".to_owned(),
            ));
        }
        if mapping
            .document_fields
            .iter()
            .filter_map(|field| body.get(field))
            .any(|value| !matches!(value, CapsuleValue::Object(_)))
        {
            return Err(PostgresStorageError::Invalid(
                "document field must hold a capsule object".to_owned(),
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
            // A302 extension: a declared relationship may be compared to one capsule ID
            // with equality only (for example "the memberships of this store").
            if mapping.relationships.contains_key(field) {
                let (ComparisonOperator::Equal, CapsuleValue::Bytes(bytes)) = (operator, value)
                else {
                    return Err(PostgresStorageError::Invalid(
                        "relationship predicates support only equality with a capsule ID"
                            .to_owned(),
                    ));
                };
                let id: [u8; 16] = bytes.as_slice().try_into().map_err(|_| {
                    PostgresStorageError::Invalid(
                        "relationship predicate needs a 16-byte capsule ID".to_owned(),
                    )
                })?;
                values.push(SqlParam::Uuid(Some(Uuid::from_bytes(id))));
                return Ok(format!("{} = ${}", sql_identifier(field), values.len()));
            }
            let column = require_query_field(mapping, field)?;
            let field_type = &mapping.fields[field];
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
    if mapping.document_fields.contains(field) {
        return Err(PostgresStorageError::Invalid(
            "document fields are not natively filterable or sortable".to_owned(),
        ));
    }
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

#[derive(Clone)]
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

pub(crate) fn map_postgres_error(error: postgres::Error) -> PostgresStorageError {
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
        assert_eq!(catalog.tables.len(), 36);
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
        assert_eq!(tables.len(), 14);
        assert_eq!(
            tables
                .iter()
                .filter(|row| row["mutation_policy"] == "append_only")
                .count(),
            6
        );
        let legacy = tables
            .iter()
            .find(|row| row["kind"] == "finance.legacy_record")
            .unwrap();
        assert_eq!(legacy["document_fields"], serde_json::json!(["document"]));
        assert!(legacy["fields"].get("document").is_none());
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

    fn document_capsule(document: CapsuleValue) -> CapsuleEnvelope {
        let capsule = CapsuleEnvelope {
            encoding_version: 1,
            id: CapsuleId([7; 16]),
            kind: CapsuleKind("core.program_metadata".to_owned()),
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Creature([3; 16]),
            schema_version: 1,
            revision: 1,
            created_at_micros: 10,
            updated_at_micros: 10,
            previous_integrity: None,
            integrity_hash: aseman_contracts::capsule::CapsuleDigest {
                algorithm: "sha2-256".to_owned(),
                bytes: vec![0; 32],
            },
            tombstone: false,
            relationships: vec![aseman_contracts::capsule::CapsuleRelationship {
                name: "program".to_owned(),
                target_kind: CapsuleKind("core.program".to_owned()),
                target_id: CapsuleId([5; 16]),
            }],
            body: Some(CapsuleValue::Object(BTreeMap::from([
                ("document".to_owned(), document),
                (
                    "document_path".to_owned(),
                    CapsuleValue::Text("metadata".to_owned()),
                ),
                ("entry_count".to_owned(), CapsuleValue::Integer(1)),
                (
                    "content_digest".to_owned(),
                    CapsuleValue::Bytes(vec![9; 32]),
                ),
            ]))),
        };
        capsule.seal().unwrap()
    }

    #[test]
    fn relationship_predicates_allow_only_capsule_id_equality() {
        let mapping = table_mapping(&CapsuleKind("core.store_membership".to_owned())).unwrap();
        let mut values = Vec::new();
        let equal = QueryPredicate::Compare {
            field: "store".to_owned(),
            operator: ComparisonOperator::Equal,
            value: CapsuleValue::Bytes(vec![7; 16]),
        };
        assert_eq!(
            predicate_sql(&equal, mapping, &mut values, 1).unwrap(),
            "\"store\" = $1"
        );
        for (operator, value) in [
            (
                ComparisonOperator::GreaterThan,
                CapsuleValue::Bytes(vec![7; 16]),
            ),
            (ComparisonOperator::Equal, CapsuleValue::Bytes(vec![7; 3])),
            (
                ComparisonOperator::Equal,
                CapsuleValue::Text("x".to_owned()),
            ),
        ] {
            let predicate = QueryPredicate::Compare {
                field: "store".to_owned(),
                operator,
                value,
            };
            assert!(predicate_sql(&predicate, mapping, &mut values, 1).is_err());
        }
    }

    #[test]
    fn document_fields_have_no_native_column_and_reject_filtering() {
        let mapping = table_mapping(&CapsuleKind("core.program_metadata".to_owned())).unwrap();
        assert!(mapping.document_fields.contains("document"));
        assert!(!mapping.fields.contains_key("document"));
        assert!(!mapping.field_columns.contains_key("document"));
        assert!(mapping.required_fields.contains("document"));
        assert!(!CORE_MIGRATION.to_ascii_lowercase().contains("jsonb"));

        assert!(require_query_field(mapping, "document").is_err());
        let mut values = Vec::new();
        assert!(
            predicate_sql(
                &QueryPredicate::Compare {
                    field: "document".to_owned(),
                    operator: ComparisonOperator::Equal,
                    value: CapsuleValue::Text("x".to_owned()),
                },
                mapping,
                &mut values,
                1,
            )
            .is_err()
        );
        assert!(values.is_empty());
    }

    #[test]
    fn document_body_is_structured_and_never_becomes_a_column() {
        let mapping = table_mapping(&CapsuleKind("core.program_metadata".to_owned())).unwrap();
        let structured = CapsuleValue::Object(BTreeMap::from([(
            "manifest".to_owned(),
            CapsuleValue::Text("mcp".to_owned()),
        )]));
        let (columns, values) =
            capsule_values(mapping, &document_capsule(structured.clone())).unwrap();
        assert!(!columns.iter().any(|column| column == "document"));
        assert_eq!(columns.len(), values.len());
        assert!(columns.iter().any(|column| column == "capsule_cbor"));

        assert!(
            capsule_values(
                mapping,
                &document_capsule(CapsuleValue::Bytes(vec![1, 2, 3])),
            )
            .is_err()
        );
        let mut undeclared = document_capsule(structured);
        if let Some(CapsuleValue::Object(body)) = undeclared.body.as_mut() {
            body.insert("stray".to_owned(), CapsuleValue::Integer(1));
        }
        assert!(capsule_values(mapping, &undeclared.seal().unwrap()).is_err());
    }
}
