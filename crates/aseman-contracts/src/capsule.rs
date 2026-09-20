//! Portable capsule values, deterministic CBOR, integrity, definitions, and queries.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

const INTEGRITY_DOMAIN: &[u8] = b"ASEMAN-CAPSULE-INTEGRITY-V1\0";
pub const ENCODING_VERSION: u16 = 1;
pub const DIGEST_ALGORITHM: &str = "sha2-256";
pub const MAX_QUERY_LIMIT: u32 = 10_000;
pub const MAX_QUERY_DEPTH: usize = 16;
pub const MAX_CAPSULE_BYTES: usize = 64 * 1024 * 1024;
const MAX_VALUE_DEPTH: usize = 64;
const MAX_COLLECTION_ITEMS: usize = 1_000_000;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CapsuleError {
    #[error("invalid capsule: {0}")]
    Invalid(String),
    #[error("unsupported capsule encoding version: {0}")]
    UnsupportedEncoding(u16),
    #[error("unsupported capsule digest algorithm: {0}")]
    UnsupportedDigest(String),
    #[error("capsule CBOR is not deterministic: {0}")]
    NonCanonical(String),
    #[error("capsule CBOR is truncated")]
    Truncated,
}

pub type CapsuleResult<T> = Result<T, CapsuleError>;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapsuleId(pub [u8; 16]);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapsuleKind(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "name")]
pub enum StorageClass {
    Core,
    GuestData,
    Telemetry,
    Audit,
    Finance,
    Outbox,
    Realtime,
    Module(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "id")]
pub enum OwnerScope {
    Global,
    Node([u8; 16]),
    Creature([u8; 16]),
    Module(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum CapsuleValue {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<CapsuleValue>),
    Object(BTreeMap<String, CapsuleValue>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleDigest {
    pub algorithm: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleRelationship {
    pub name: String,
    pub target_kind: CapsuleKind,
    pub target_id: CapsuleId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleEnvelope {
    pub encoding_version: u16,
    pub id: CapsuleId,
    pub kind: CapsuleKind,
    pub storage_class: StorageClass,
    pub owner_scope: OwnerScope,
    pub schema_version: u32,
    pub revision: u64,
    pub created_at_micros: i64,
    pub updated_at_micros: i64,
    pub previous_integrity: Option<CapsuleDigest>,
    pub integrity_hash: CapsuleDigest,
    pub tombstone: bool,
    pub relationships: Vec<CapsuleRelationship>,
    pub body: Option<CapsuleValue>,
}

impl CapsuleEnvelope {
    pub fn seal(mut self) -> CapsuleResult<Self> {
        self.validate_structure(false)?;
        self.integrity_hash = self.compute_integrity()?;
        Ok(self)
    }

    pub fn verify(&self) -> CapsuleResult<()> {
        self.validate_structure(true)?;
        let expected = self.compute_integrity()?;
        if expected != self.integrity_hash {
            return Err(CapsuleError::Invalid("integrity hash mismatch".to_owned()));
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> CapsuleResult<Vec<u8>> {
        self.verify()?;
        encode_value(&self.as_value(true))
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> CapsuleResult<Self> {
        let value = decode_canonical_value(bytes)?;
        let envelope = Self::from_value(value)?;
        envelope.verify()?;
        Ok(envelope)
    }

    pub fn compute_integrity(&self) -> CapsuleResult<CapsuleDigest> {
        if self.encoding_version != ENCODING_VERSION {
            return Err(CapsuleError::UnsupportedEncoding(self.encoding_version));
        }
        let encoded = encode_value(&self.as_value(false))?;
        let mut hasher = Sha256::new();
        hasher.update(INTEGRITY_DOMAIN);
        hasher.update(ENCODING_VERSION.to_be_bytes());
        hasher.update(DIGEST_ALGORITHM.as_bytes());
        hasher.update([0]);
        hasher.update(encoded);
        Ok(CapsuleDigest {
            algorithm: DIGEST_ALGORITHM.to_owned(),
            bytes: hasher.finalize().to_vec(),
        })
    }

    fn validate_structure(&self, validate_current_integrity: bool) -> CapsuleResult<()> {
        validate_token("kind", &self.kind.0)?;
        if self.encoding_version != ENCODING_VERSION {
            return Err(CapsuleError::UnsupportedEncoding(self.encoding_version));
        }
        if self.schema_version == 0 || self.revision == 0 {
            return Err(CapsuleError::Invalid(
                "schema_version and revision start at one".to_owned(),
            ));
        }
        if self.updated_at_micros < self.created_at_micros {
            return Err(CapsuleError::Invalid(
                "updated_at precedes created_at".to_owned(),
            ));
        }
        if self.tombstone != self.body.is_none() {
            return Err(CapsuleError::Invalid(
                "tombstones have no body and live revisions require a body".to_owned(),
            ));
        }
        if (self.revision == 1) != self.previous_integrity.is_none() {
            return Err(CapsuleError::Invalid(
                "only the first revision omits previous_integrity".to_owned(),
            ));
        }
        if validate_current_integrity {
            validate_digest(&self.integrity_hash)?;
        }
        if let Some(previous) = &self.previous_integrity {
            validate_digest(previous)?;
        }
        let mut relationship_names = BTreeSet::new();
        for relationship in &self.relationships {
            validate_token("relationship name", &relationship.name)?;
            validate_token("relationship target kind", &relationship.target_kind.0)?;
            if !relationship_names.insert(&relationship.name) {
                return Err(CapsuleError::Invalid(
                    "relationship names must be unique".to_owned(),
                ));
            }
        }
        validate_value(self.body.as_ref())
    }

    fn as_value(&self, include_integrity: bool) -> CapsuleValue {
        let mut map = BTreeMap::from([
            ("body".to_owned(), option_value(self.body.clone())),
            (
                "created_at_micros".to_owned(),
                integer(self.created_at_micros),
            ),
            (
                "encoding_version".to_owned(),
                integer(self.encoding_version),
            ),
            ("id".to_owned(), CapsuleValue::Bytes(self.id.0.to_vec())),
            ("kind".to_owned(), CapsuleValue::Text(self.kind.0.clone())),
            ("owner_scope".to_owned(), owner_value(&self.owner_scope)),
            (
                "previous_integrity".to_owned(),
                option_value(self.previous_integrity.as_ref().map(digest_value)),
            ),
            (
                "relationships".to_owned(),
                CapsuleValue::Array(self.relationships.iter().map(relationship_value).collect()),
            ),
            ("revision".to_owned(), integer(self.revision)),
            ("schema_version".to_owned(), integer(self.schema_version)),
            (
                "storage_class".to_owned(),
                storage_class_value(&self.storage_class),
            ),
            ("tombstone".to_owned(), CapsuleValue::Bool(self.tombstone)),
            (
                "updated_at_micros".to_owned(),
                integer(self.updated_at_micros),
            ),
        ]);
        if include_integrity {
            map.insert(
                "integrity_hash".to_owned(),
                digest_value(&self.integrity_hash),
            );
        }
        CapsuleValue::Object(map)
    }

    fn from_value(value: CapsuleValue) -> CapsuleResult<Self> {
        let mut map = into_object(value, "capsule envelope")?;
        reject_unknown(
            &map,
            &[
                "body",
                "created_at_micros",
                "encoding_version",
                "id",
                "integrity_hash",
                "kind",
                "owner_scope",
                "previous_integrity",
                "relationships",
                "revision",
                "schema_version",
                "storage_class",
                "tombstone",
                "updated_at_micros",
            ],
        )?;
        Ok(Self {
            encoding_version: take_u64(&mut map, "encoding_version")?
                .try_into()
                .map_err(|_| invalid("encoding_version overflow"))?,
            id: CapsuleId(take_fixed_bytes::<16>(&mut map, "id")?),
            kind: CapsuleKind(take_text(&mut map, "kind")?),
            storage_class: parse_storage_class(take(&mut map, "storage_class")?)?,
            owner_scope: parse_owner(take(&mut map, "owner_scope")?)?,
            schema_version: take_u64(&mut map, "schema_version")?
                .try_into()
                .map_err(|_| invalid("schema_version overflow"))?,
            revision: take_u64(&mut map, "revision")?,
            created_at_micros: take_i64(&mut map, "created_at_micros")?,
            updated_at_micros: take_i64(&mut map, "updated_at_micros")?,
            previous_integrity: parse_optional(
                take(&mut map, "previous_integrity")?,
                parse_digest,
            )?,
            integrity_hash: parse_digest(take(&mut map, "integrity_hash")?)?,
            tombstone: take_bool(&mut map, "tombstone")?,
            relationships: into_array(take(&mut map, "relationships")?, "relationships")?
                .into_iter()
                .map(parse_relationship)
                .collect::<CapsuleResult<_>>()?,
            body: parse_optional(take(&mut map, "body")?, Ok)?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsistencyProfile {
    Serializable,
    Snapshot,
    AppendLinearizable,
    ReadCommitted,
    Eventual,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleDefinition {
    pub kind: CapsuleKind,
    pub schema_version: u32,
    pub storage_class: StorageClass,
    pub consistency: ConsistencyProfile,
    pub required_fields: BTreeSet<String>,
    pub fields: BTreeMap<String, FieldDefinition>,
    pub indexes: Vec<IndexDefinition>,
    pub relationships: BTreeMap<String, RelationshipDefinition>,
}

impl CapsuleDefinition {
    pub fn validate(&self) -> CapsuleResult<()> {
        validate_token("capsule kind", &self.kind.0)?;
        if self.schema_version == 0 || self.fields.is_empty() {
            return Err(invalid("definition version and fields are required"));
        }
        for required in &self.required_fields {
            let field = self
                .fields
                .get(required)
                .ok_or_else(|| invalid("required field is not declared"))?;
            if field.nullable {
                return Err(invalid("required fields cannot be nullable"));
            }
        }
        let mut index_names = BTreeSet::new();
        for index in &self.indexes {
            validate_token("index name", &index.name)?;
            if !index_names.insert(&index.name) || index.fields.is_empty() {
                return Err(invalid("index names must be unique and contain fields"));
            }
            for field in &index.fields {
                if !self.fields.contains_key(field) {
                    return Err(invalid("index references an undeclared field"));
                }
            }
        }
        for (name, relationship) in &self.relationships {
            validate_token("relationship name", name)?;
            validate_token("relationship target kind", &relationship.target_kind.0)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Bool,
    Integer,
    Float,
    Bytes,
    Text,
    TimestampMicros,
    CapsuleId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDefinition {
    pub field_type: FieldType,
    pub searchable: bool,
    pub sortable: bool,
    pub nullable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexDefinition {
    pub name: String,
    pub fields: Vec<String>,
    pub unique: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipDefinition {
    pub target_kind: CapsuleKind,
    pub required: bool,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum StorageCapability {
    #[serde(rename = "transactions.single_capsule")]
    TransactionsSingleCapsule,
    #[serde(rename = "transactions.multi_capsule")]
    TransactionsMultiCapsule,
    #[serde(rename = "relationships.foreign_keys")]
    RelationshipsForeignKeys,
    #[serde(rename = "queries.range")]
    QueriesRange,
    #[serde(rename = "queries.full_text")]
    QueriesFullText,
    #[serde(rename = "queries.relationship_traversal")]
    QueriesRelationshipTraversal,
    #[serde(rename = "indexes.unique")]
    IndexesUnique,
    #[serde(rename = "events.change_stream")]
    EventsChangeStream,
    #[serde(rename = "consistency.linearizable")]
    ConsistencyLinearizable,
    #[serde(rename = "consistency.snapshot")]
    ConsistencySnapshot,
    #[serde(rename = "consistency.read_committed")]
    ConsistencyReadCommitted,
    #[serde(rename = "consistency.eventual")]
    ConsistencyEventual,
    #[serde(rename = "append_only.verifiable")]
    AppendOnlyVerifiable,
    #[serde(rename = "guest_database.isolated_roles")]
    GuestDatabaseIsolatedRoles,
    #[serde(rename = "guest_database.catalog_isolation")]
    GuestDatabaseCatalogIsolation,
    #[serde(rename = "guest_database.schema_management")]
    GuestDatabaseSchemaManagement,
    #[serde(rename = "guest_database.safe_role_assumption")]
    GuestDatabaseSafeRoleAssumption,
}

impl StorageCapability {
    #[must_use]
    pub const fn wire_name(&self) -> &'static str {
        match self {
            Self::TransactionsSingleCapsule => "transactions.single_capsule",
            Self::TransactionsMultiCapsule => "transactions.multi_capsule",
            Self::RelationshipsForeignKeys => "relationships.foreign_keys",
            Self::QueriesRange => "queries.range",
            Self::QueriesFullText => "queries.full_text",
            Self::QueriesRelationshipTraversal => "queries.relationship_traversal",
            Self::IndexesUnique => "indexes.unique",
            Self::EventsChangeStream => "events.change_stream",
            Self::ConsistencyLinearizable => "consistency.linearizable",
            Self::ConsistencySnapshot => "consistency.snapshot",
            Self::ConsistencyReadCommitted => "consistency.read_committed",
            Self::ConsistencyEventual => "consistency.eventual",
            Self::AppendOnlyVerifiable => "append_only.verifiable",
            Self::GuestDatabaseIsolatedRoles => "guest_database.isolated_roles",
            Self::GuestDatabaseCatalogIsolation => "guest_database.catalog_isolation",
            Self::GuestDatabaseSchemaManagement => "guest_database.schema_management",
            Self::GuestDatabaseSafeRoleAssumption => "guest_database.safe_role_assumption",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCapabilities {
    pub provider_id: String,
    pub capabilities: BTreeSet<StorageCapability>,
    pub max_query_limit: u32,
    pub max_transaction_capsules: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityReport {
    pub compatible: bool,
    pub missing: BTreeSet<StorageCapability>,
}

pub fn required_capabilities(definition: &CapsuleDefinition) -> BTreeSet<StorageCapability> {
    let mut required = BTreeSet::from([StorageCapability::TransactionsSingleCapsule]);
    required.insert(match definition.consistency {
        ConsistencyProfile::Serializable | ConsistencyProfile::AppendLinearizable => {
            StorageCapability::ConsistencyLinearizable
        }
        ConsistencyProfile::Snapshot => StorageCapability::ConsistencySnapshot,
        ConsistencyProfile::ReadCommitted => StorageCapability::ConsistencyReadCommitted,
        ConsistencyProfile::Eventual => StorageCapability::ConsistencyEventual,
    });
    if definition.consistency == ConsistencyProfile::AppendLinearizable {
        required.insert(StorageCapability::AppendOnlyVerifiable);
    }
    if !definition.relationships.is_empty() {
        required.insert(StorageCapability::RelationshipsForeignKeys);
    }
    if definition.indexes.iter().any(|index| index.unique) {
        required.insert(StorageCapability::IndexesUnique);
    }
    if definition.storage_class == StorageClass::GuestData {
        required.extend([
            StorageCapability::GuestDatabaseIsolatedRoles,
            StorageCapability::GuestDatabaseCatalogIsolation,
            StorageCapability::GuestDatabaseSchemaManagement,
            StorageCapability::GuestDatabaseSafeRoleAssumption,
        ]);
    }
    required
}

pub fn negotiate_capabilities(
    definition: &CapsuleDefinition,
    provider: &ProviderCapabilities,
) -> CapsuleResult<CapabilityReport> {
    definition.validate()?;
    validate_token("provider_id", &provider.provider_id)?;
    if provider.max_query_limit == 0 || provider.max_transaction_capsules == 0 {
        return Err(invalid("provider limits must be positive"));
    }
    let required = required_capabilities(definition);
    let missing = required
        .difference(&provider.capabilities)
        .cloned()
        .collect::<BTreeSet<_>>();
    Ok(CapabilityReport {
        compatible: missing.is_empty(),
        missing,
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonOperator {
    Equal,
    NotEqual,
    LessThan,
    LessOrEqual,
    GreaterThan,
    GreaterOrEqual,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum QueryPredicate {
    Compare {
        field: String,
        operator: ComparisonOperator,
        value: CapsuleValue,
    },
    And {
        predicates: Vec<QueryPredicate>,
    },
    Or {
        predicates: Vec<QueryPredicate>,
    },
    Not {
        predicate: Box<QueryPredicate>,
    },
    RelationshipExists {
        relationship: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "function")]
pub enum QueryAggregate {
    Count { alias: String },
    Sum { field: String, alias: String },
    Minimum { field: String, alias: String },
    Maximum { field: String, alias: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipTraversal {
    pub relationship: String,
    pub target_kind: CapsuleKind,
    pub projection: BTreeSet<String>,
    pub limit: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryCursor {
    pub version: u16,
    pub kind: CapsuleKind,
    pub query_digest: CapsuleDigest,
    pub position: Vec<u8>,
    pub expires_at_micros: i64,
    pub signature: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryErrorCode {
    InvalidQuery,
    UnsupportedCapability,
    RevisionConflict,
    CursorInvalid,
    CursorExpired,
    Unavailable,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryError {
    pub code: QueryErrorCode,
    pub message: String,
    pub retryable: bool,
    pub missing_capabilities: BTreeSet<StorageCapability>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuerySort {
    pub field: String,
    pub direction: SortDirection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleQuery {
    pub kind: CapsuleKind,
    pub predicate: Option<QueryPredicate>,
    pub projection: BTreeSet<String>,
    pub sort: Vec<QuerySort>,
    pub aggregates: Vec<QueryAggregate>,
    pub traversals: Vec<RelationshipTraversal>,
    pub limit: u32,
    pub cursor: Option<QueryCursor>,
}

impl CapsuleQuery {
    pub fn validate(&self, definition: &CapsuleDefinition) -> CapsuleResult<()> {
        definition.validate()?;
        if self.kind != definition.kind || self.limit == 0 || self.limit > MAX_QUERY_LIMIT {
            return Err(invalid("query kind or limit is invalid"));
        }
        for field in &self.projection {
            require_field(definition, field)?;
        }
        for sort in &self.sort {
            let field = require_field(definition, &sort.field)?;
            if !field.sortable {
                return Err(invalid("query sort field is not sortable"));
            }
        }
        for aggregate in &self.aggregates {
            let (field, alias) = match aggregate {
                QueryAggregate::Count { alias } => (None, alias),
                QueryAggregate::Sum { field, alias } => {
                    let definition = require_field(definition, field)?;
                    if !definition.searchable
                        || !matches!(definition.field_type, FieldType::Integer | FieldType::Float)
                    {
                        return Err(invalid("sum requires a searchable numeric field"));
                    }
                    (None, alias)
                }
                QueryAggregate::Minimum { field, alias }
                | QueryAggregate::Maximum { field, alias } => (Some(field), alias),
            };
            validate_token("aggregate alias", alias)?;
            if let Some(field) = field {
                let field = require_field(definition, field)?;
                if !field.searchable {
                    return Err(invalid("aggregate field is not searchable"));
                }
            }
        }
        for traversal in &self.traversals {
            if traversal.limit == 0 || traversal.limit > MAX_QUERY_LIMIT {
                return Err(invalid("relationship traversal limit is invalid"));
            }
            let relationship = definition
                .relationships
                .get(&traversal.relationship)
                .ok_or_else(|| invalid("query references an undeclared relationship"))?;
            if relationship.target_kind != traversal.target_kind {
                return Err(invalid("relationship target kind does not match"));
            }
        }
        if let Some(cursor) = &self.cursor {
            if cursor.version != 1
                || cursor.kind != self.kind
                || cursor.expires_at_micros <= 0
                || cursor.signature.is_empty()
            {
                return Err(invalid("query cursor binding is invalid"));
            }
            validate_digest(&cursor.query_digest)?;
        }
        if let Some(predicate) = &self.predicate {
            validate_predicate(predicate, definition, 1)?;
        }
        Ok(())
    }

    pub fn validate_with_registry(
        &self,
        definitions: &BTreeMap<CapsuleKind, CapsuleDefinition>,
    ) -> CapsuleResult<()> {
        let definition = definitions
            .get(&self.kind)
            .ok_or_else(|| invalid("query kind is not registered"))?;
        self.validate(definition)?;
        for traversal in &self.traversals {
            let target = definitions
                .get(&traversal.target_kind)
                .ok_or_else(|| invalid("relationship target kind is not registered"))?;
            target.validate()?;
            for field in &traversal.projection {
                require_field(target, field)?;
            }
        }
        Ok(())
    }
}

fn validate_predicate(
    predicate: &QueryPredicate,
    definition: &CapsuleDefinition,
    depth: usize,
) -> CapsuleResult<()> {
    if depth > MAX_QUERY_DEPTH {
        return Err(invalid("query predicate nesting exceeds the limit"));
    }
    match predicate {
        QueryPredicate::Compare { field, value, .. } => {
            let field = require_field(definition, field)?;
            if !field.searchable {
                return Err(invalid("query field is not searchable"));
            }
            validate_value(Some(value))?;
            if !field_accepts_value(field, value) {
                return Err(invalid(
                    "query value does not match the declared field type",
                ));
            }
            Ok(())
        }
        QueryPredicate::And { predicates } | QueryPredicate::Or { predicates } => {
            if predicates.is_empty() || predicates.len() > 64 {
                return Err(invalid("boolean predicate width is invalid"));
            }
            for child in predicates {
                validate_predicate(child, definition, depth + 1)?;
            }
            Ok(())
        }
        QueryPredicate::Not { predicate } => validate_predicate(predicate, definition, depth + 1),
        QueryPredicate::RelationshipExists { relationship } => {
            if definition.relationships.contains_key(relationship) {
                Ok(())
            } else {
                Err(invalid("query references an undeclared relationship"))
            }
        }
    }
}

fn field_accepts_value(field: &FieldDefinition, value: &CapsuleValue) -> bool {
    if matches!(value, CapsuleValue::Null) {
        return field.nullable;
    }
    matches!(
        (&field.field_type, value),
        (FieldType::Bool, CapsuleValue::Bool(_))
            | (
                FieldType::Integer | FieldType::TimestampMicros,
                CapsuleValue::Integer(_)
            )
            | (
                FieldType::Float,
                CapsuleValue::Float(_) | CapsuleValue::Integer(_)
            )
            | (
                FieldType::Bytes | FieldType::CapsuleId,
                CapsuleValue::Bytes(_)
            )
            | (FieldType::Text, CapsuleValue::Text(_))
    ) && (!matches!(field.field_type, FieldType::CapsuleId)
        || matches!(value, CapsuleValue::Bytes(bytes) if bytes.len() == 16))
}

fn require_field<'a>(
    definition: &'a CapsuleDefinition,
    name: &str,
) -> CapsuleResult<&'a FieldDefinition> {
    definition
        .fields
        .get(name)
        .ok_or_else(|| invalid("query references an undeclared field"))
}

pub fn encode_value(value: &CapsuleValue) -> CapsuleResult<Vec<u8>> {
    validate_value(Some(value))?;
    let mut output = Vec::new();
    encode_into(value, &mut output)?;
    Ok(output)
}

pub fn decode_canonical_value(bytes: &[u8]) -> CapsuleResult<CapsuleValue> {
    if bytes.len() > MAX_CAPSULE_BYTES {
        return Err(CapsuleError::NonCanonical(
            "capsule exceeds the byte limit".to_owned(),
        ));
    }
    let mut decoder = Decoder { bytes, offset: 0 };
    let value = decoder.value(0)?;
    if decoder.offset != bytes.len() {
        return Err(CapsuleError::NonCanonical("trailing bytes".to_owned()));
    }
    if encode_value(&value)? != bytes {
        return Err(CapsuleError::NonCanonical(
            "value does not use the preferred encoding".to_owned(),
        ));
    }
    Ok(value)
}

fn encode_into(value: &CapsuleValue, output: &mut Vec<u8>) -> CapsuleResult<()> {
    match value {
        CapsuleValue::Null => output.push(0xf6),
        CapsuleValue::Bool(false) => output.push(0xf4),
        CapsuleValue::Bool(true) => output.push(0xf5),
        CapsuleValue::Integer(value) if *value >= 0 => encode_head(0, *value as u64, output),
        CapsuleValue::Integer(value) => {
            encode_head(1, (-1_i128 - i128::from(*value)) as u64, output)
        }
        CapsuleValue::Float(value) => encode_float(*value, output)?,
        CapsuleValue::Bytes(value) => {
            encode_head(2, value.len() as u64, output);
            output.extend_from_slice(value);
        }
        CapsuleValue::Text(value) => {
            encode_head(3, value.len() as u64, output);
            output.extend_from_slice(value.as_bytes());
        }
        CapsuleValue::Array(values) => {
            encode_head(4, values.len() as u64, output);
            for value in values {
                encode_into(value, output)?;
            }
        }
        CapsuleValue::Object(values) => {
            let mut entries = values
                .iter()
                .map(|(key, value)| {
                    let key = encode_value(&CapsuleValue::Text(key.clone()))?;
                    Ok((key, value))
                })
                .collect::<CapsuleResult<Vec<_>>>()?;
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            encode_head(5, entries.len() as u64, output);
            for (key, value) in entries {
                output.extend_from_slice(&key);
                encode_into(value, output)?;
            }
        }
    }
    Ok(())
}

fn encode_head(major: u8, value: u64, output: &mut Vec<u8>) {
    let prefix = major << 5;
    match value {
        0..=23 => output.push(prefix | value as u8),
        24..=0xff => output.extend_from_slice(&[prefix | 24, value as u8]),
        0x100..=0xffff => {
            output.push(prefix | 25);
            output.extend_from_slice(&(value as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            output.push(prefix | 26);
            output.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            output.push(prefix | 27);
            output.extend_from_slice(&value.to_be_bytes());
        }
    }
}

fn encode_float(value: f64, output: &mut Vec<u8>) -> CapsuleResult<()> {
    if !value.is_finite() {
        return Err(invalid("non-finite floats are forbidden"));
    }
    if let Some(bits) = exact_f16(value) {
        output.push(0xf9);
        output.extend_from_slice(&bits.to_be_bytes());
    } else if (value as f32) as f64 == value {
        output.push(0xfa);
        output.extend_from_slice(&(value as f32).to_bits().to_be_bytes());
    } else {
        output.push(0xfb);
        output.extend_from_slice(&value.to_bits().to_be_bytes());
    }
    Ok(())
}

fn exact_f16(value: f64) -> Option<u16> {
    if (value as f32) as f64 != value {
        return None;
    }
    let bits = f32_to_f16_bits(value as f32);
    (f16_bits_to_f32(bits) as f64 == value).then_some(bits)
}

fn f32_to_f16_bits(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7f_ffff;
    if exponent <= 0 {
        if exponent < -10 {
            return sign;
        }
        let mantissa = mantissa | 0x80_0000;
        let shift = 14 - exponent;
        let mut half = (mantissa >> shift) as u16;
        let remainder = mantissa & ((1_u32 << shift) - 1);
        let halfway = 1_u32 << (shift - 1);
        if remainder > halfway || (remainder == halfway && half & 1 == 1) {
            half += 1;
        }
        return sign | half;
    }
    if exponent >= 31 {
        return sign | 0x7c00;
    }
    let mut half = sign | ((exponent as u16) << 10) | ((mantissa >> 13) as u16);
    let remainder = mantissa & 0x1fff;
    if remainder > 0x1000 || (remainder == 0x1000 && half & 1 == 1) {
        half += 1;
    }
    half
}

fn f16_bits_to_f32(bits: u16) -> f32 {
    let sign = ((bits & 0x8000) as u32) << 16;
    let exponent = (bits >> 10) & 0x1f;
    let mantissa = (bits & 0x03ff) as u32;
    let value = match exponent {
        0 if mantissa == 0 => sign,
        0 => {
            let mut mantissa = mantissa;
            let mut exponent = -14_i32;
            while mantissa & 0x400 == 0 {
                mantissa <<= 1;
                exponent -= 1;
            }
            mantissa &= 0x3ff;
            sign | (((exponent + 127) as u32) << 23) | (mantissa << 13)
        }
        31 => sign | 0x7f80_0000 | (mantissa << 13),
        _ => sign | (((i32::from(exponent) - 15 + 127) as u32) << 23) | (mantissa << 13),
    };
    f32::from_bits(value)
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Decoder<'_> {
    fn value(&mut self, depth: usize) -> CapsuleResult<CapsuleValue> {
        if depth > MAX_VALUE_DEPTH {
            return Err(CapsuleError::NonCanonical(
                "capsule nesting exceeds the limit".to_owned(),
            ));
        }
        let initial = self.byte()?;
        let major = initial >> 5;
        let additional = initial & 0x1f;
        match major {
            0 => Ok(CapsuleValue::Integer(
                self.length(additional)?
                    .try_into()
                    .map_err(|_| invalid("integer overflow"))?,
            )),
            1 => {
                let value = self.length(additional)?;
                let value = -1_i128 - i128::from(value);
                Ok(CapsuleValue::Integer(
                    value.try_into().map_err(|_| invalid("integer overflow"))?,
                ))
            }
            2 => {
                let length = self.length(additional)? as usize;
                Ok(CapsuleValue::Bytes(self.take(length)?.to_vec()))
            }
            3 => {
                let length = self.length(additional)? as usize;
                let text = std::str::from_utf8(self.take(length)?)
                    .map_err(|_| invalid("CBOR text is not UTF-8"))?;
                Ok(CapsuleValue::Text(text.to_owned()))
            }
            4 => {
                let length = self.collection_length(additional)?;
                let mut values = Vec::with_capacity(length);
                for _ in 0..length {
                    values.push(self.value(depth + 1)?);
                }
                Ok(CapsuleValue::Array(values))
            }
            5 => {
                let length = self.collection_length(additional)?;
                let mut values = BTreeMap::new();
                for _ in 0..length {
                    let CapsuleValue::Text(key) = self.value(depth + 1)? else {
                        return Err(invalid("capsule map keys must be text"));
                    };
                    if values.insert(key, self.value(depth + 1)?).is_some() {
                        return Err(CapsuleError::NonCanonical("duplicate map key".to_owned()));
                    }
                }
                Ok(CapsuleValue::Object(values))
            }
            7 => match additional {
                20 => Ok(CapsuleValue::Bool(false)),
                21 => Ok(CapsuleValue::Bool(true)),
                22 => Ok(CapsuleValue::Null),
                25 => Ok(CapsuleValue::Float(f16_bits_to_f32(self.u16()?) as f64)),
                26 => Ok(CapsuleValue::Float(f32::from_bits(self.u32()?) as f64)),
                27 => Ok(CapsuleValue::Float(f64::from_bits(self.u64()?))),
                _ => Err(CapsuleError::NonCanonical(
                    "unsupported simple value".to_owned(),
                )),
            },
            _ => Err(CapsuleError::NonCanonical(
                "unsupported CBOR major type".to_owned(),
            )),
        }
    }

    fn length(&mut self, additional: u8) -> CapsuleResult<u64> {
        match additional {
            0..=23 => Ok(u64::from(additional)),
            24 => Ok(u64::from(self.byte()?)),
            25 => Ok(u64::from(self.u16()?)),
            26 => Ok(u64::from(self.u32()?)),
            27 => self.u64(),
            _ => Err(CapsuleError::NonCanonical(
                "indefinite lengths are forbidden".to_owned(),
            )),
        }
    }

    fn collection_length(&mut self, additional: u8) -> CapsuleResult<usize> {
        let length: usize = self
            .length(additional)?
            .try_into()
            .map_err(|_| invalid("collection length overflow"))?;
        if length > MAX_COLLECTION_ITEMS || length > self.bytes.len().saturating_sub(self.offset) {
            return Err(CapsuleError::NonCanonical(
                "collection exceeds the item or input bound".to_owned(),
            ));
        }
        Ok(length)
    }

    fn byte(&mut self) -> CapsuleResult<u8> {
        Ok(*self.take(1)?.first().ok_or(CapsuleError::Truncated)?)
    }

    fn u16(&mut self) -> CapsuleResult<u16> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| CapsuleError::Truncated)?,
        ))
    }

    fn u32(&mut self) -> CapsuleResult<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| CapsuleError::Truncated)?,
        ))
    }

    fn u64(&mut self) -> CapsuleResult<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| CapsuleError::Truncated)?,
        ))
    }

    fn take(&mut self, length: usize) -> CapsuleResult<&[u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(CapsuleError::Truncated)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(CapsuleError::Truncated)?;
        self.offset = end;
        Ok(value)
    }
}

fn validate_value(value: Option<&CapsuleValue>) -> CapsuleResult<()> {
    validate_value_at(value, 0)
}

fn validate_value_at(value: Option<&CapsuleValue>, depth: usize) -> CapsuleResult<()> {
    if depth > MAX_VALUE_DEPTH {
        return Err(invalid("capsule value nesting exceeds the limit"));
    }
    match value {
        None
        | Some(
            CapsuleValue::Null
            | CapsuleValue::Bool(_)
            | CapsuleValue::Integer(_)
            | CapsuleValue::Bytes(_)
            | CapsuleValue::Text(_),
        ) => Ok(()),
        Some(CapsuleValue::Float(value)) if value.is_finite() => Ok(()),
        Some(CapsuleValue::Float(_)) => Err(invalid("non-finite floats are forbidden")),
        Some(CapsuleValue::Array(values)) => {
            if values.len() > MAX_COLLECTION_ITEMS {
                return Err(invalid("capsule array exceeds the item limit"));
            }
            for value in values {
                validate_value_at(Some(value), depth + 1)?;
            }
            Ok(())
        }
        Some(CapsuleValue::Object(values)) => {
            if values.len() > MAX_COLLECTION_ITEMS {
                return Err(invalid("capsule object exceeds the item limit"));
            }
            for (key, value) in values {
                if key.is_empty() {
                    return Err(invalid("object keys cannot be empty"));
                }
                validate_value_at(Some(value), depth + 1)?;
            }
            Ok(())
        }
    }
}

fn validate_token(label: &str, value: &str) -> CapsuleResult<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(invalid(&format!("{label} is not a safe token")));
    }
    Ok(())
}

fn validate_digest(digest: &CapsuleDigest) -> CapsuleResult<()> {
    if digest.algorithm != DIGEST_ALGORITHM {
        return Err(CapsuleError::UnsupportedDigest(digest.algorithm.clone()));
    }
    if digest.bytes.len() != 32 {
        return Err(invalid("sha2-256 digest must be 32 bytes"));
    }
    Ok(())
}

fn invalid(message: &str) -> CapsuleError {
    CapsuleError::Invalid(message.to_owned())
}

fn integer<T: Into<i128>>(value: T) -> CapsuleValue {
    CapsuleValue::Integer(i64::try_from(value.into()).expect("capsule integer fits i64"))
}

fn option_value(value: Option<CapsuleValue>) -> CapsuleValue {
    value.unwrap_or(CapsuleValue::Null)
}

fn digest_value(digest: &CapsuleDigest) -> CapsuleValue {
    CapsuleValue::Object(BTreeMap::from([
        (
            "algorithm".to_owned(),
            CapsuleValue::Text(digest.algorithm.clone()),
        ),
        (
            "bytes".to_owned(),
            CapsuleValue::Bytes(digest.bytes.clone()),
        ),
    ]))
}

fn relationship_value(relationship: &CapsuleRelationship) -> CapsuleValue {
    CapsuleValue::Object(BTreeMap::from([
        (
            "name".to_owned(),
            CapsuleValue::Text(relationship.name.clone()),
        ),
        (
            "target_id".to_owned(),
            CapsuleValue::Bytes(relationship.target_id.0.to_vec()),
        ),
        (
            "target_kind".to_owned(),
            CapsuleValue::Text(relationship.target_kind.0.clone()),
        ),
    ]))
}

fn storage_class_value(class: &StorageClass) -> CapsuleValue {
    let (kind, name) = match class {
        StorageClass::Core => ("core", None),
        StorageClass::GuestData => ("guest_data", None),
        StorageClass::Telemetry => ("telemetry", None),
        StorageClass::Audit => ("audit", None),
        StorageClass::Finance => ("finance", None),
        StorageClass::Outbox => ("outbox", None),
        StorageClass::Realtime => ("realtime", None),
        StorageClass::Module(name) => ("module", Some(name.clone())),
    };
    CapsuleValue::Object(BTreeMap::from([
        (
            "name".to_owned(),
            option_value(name.map(CapsuleValue::Text)),
        ),
        ("type".to_owned(), CapsuleValue::Text(kind.to_owned())),
    ]))
}

fn owner_value(owner: &OwnerScope) -> CapsuleValue {
    let (kind, id) = match owner {
        OwnerScope::Global => ("global", CapsuleValue::Null),
        OwnerScope::Node(id) => ("node", CapsuleValue::Bytes(id.to_vec())),
        OwnerScope::Creature(id) => ("creature", CapsuleValue::Bytes(id.to_vec())),
        OwnerScope::Module(name) => ("module", CapsuleValue::Text(name.clone())),
    };
    CapsuleValue::Object(BTreeMap::from([
        ("id".to_owned(), id),
        ("type".to_owned(), CapsuleValue::Text(kind.to_owned())),
    ]))
}

fn take(map: &mut BTreeMap<String, CapsuleValue>, key: &str) -> CapsuleResult<CapsuleValue> {
    map.remove(key)
        .ok_or_else(|| invalid(&format!("missing {key}")))
}

fn take_text(map: &mut BTreeMap<String, CapsuleValue>, key: &str) -> CapsuleResult<String> {
    match take(map, key)? {
        CapsuleValue::Text(value) => Ok(value),
        _ => Err(invalid(&format!("{key} must be text"))),
    }
}

fn take_i64(map: &mut BTreeMap<String, CapsuleValue>, key: &str) -> CapsuleResult<i64> {
    match take(map, key)? {
        CapsuleValue::Integer(value) => Ok(value),
        _ => Err(invalid(&format!("{key} must be an integer"))),
    }
}

fn take_u64(map: &mut BTreeMap<String, CapsuleValue>, key: &str) -> CapsuleResult<u64> {
    take_i64(map, key)?
        .try_into()
        .map_err(|_| invalid(&format!("{key} must be non-negative")))
}

fn take_bool(map: &mut BTreeMap<String, CapsuleValue>, key: &str) -> CapsuleResult<bool> {
    match take(map, key)? {
        CapsuleValue::Bool(value) => Ok(value),
        _ => Err(invalid(&format!("{key} must be a boolean"))),
    }
}

fn take_fixed_bytes<const N: usize>(
    map: &mut BTreeMap<String, CapsuleValue>,
    key: &str,
) -> CapsuleResult<[u8; N]> {
    match take(map, key)? {
        CapsuleValue::Bytes(value) => value
            .try_into()
            .map_err(|_| invalid(&format!("{key} has the wrong length"))),
        _ => Err(invalid(&format!("{key} must be bytes"))),
    }
}

fn into_object(value: CapsuleValue, label: &str) -> CapsuleResult<BTreeMap<String, CapsuleValue>> {
    match value {
        CapsuleValue::Object(value) => Ok(value),
        _ => Err(invalid(&format!("{label} must be an object"))),
    }
}

fn into_array(value: CapsuleValue, label: &str) -> CapsuleResult<Vec<CapsuleValue>> {
    match value {
        CapsuleValue::Array(value) => Ok(value),
        _ => Err(invalid(&format!("{label} must be an array"))),
    }
}

fn reject_unknown(map: &BTreeMap<String, CapsuleValue>, allowed: &[&str]) -> CapsuleResult<()> {
    if map.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid("object contains an unknown field"));
    }
    Ok(())
}

fn parse_optional<T>(
    value: CapsuleValue,
    parse: impl FnOnce(CapsuleValue) -> CapsuleResult<T>,
) -> CapsuleResult<Option<T>> {
    match value {
        CapsuleValue::Null => Ok(None),
        value => parse(value).map(Some),
    }
}

fn parse_digest(value: CapsuleValue) -> CapsuleResult<CapsuleDigest> {
    let mut map = into_object(value, "digest")?;
    reject_unknown(&map, &["algorithm", "bytes"])?;
    let digest = CapsuleDigest {
        algorithm: take_text(&mut map, "algorithm")?,
        bytes: match take(&mut map, "bytes")? {
            CapsuleValue::Bytes(value) => value,
            _ => return Err(invalid("digest bytes must be bytes")),
        },
    };
    validate_digest(&digest)?;
    Ok(digest)
}

fn parse_relationship(value: CapsuleValue) -> CapsuleResult<CapsuleRelationship> {
    let mut map = into_object(value, "relationship")?;
    reject_unknown(&map, &["name", "target_id", "target_kind"])?;
    Ok(CapsuleRelationship {
        name: take_text(&mut map, "name")?,
        target_kind: CapsuleKind(take_text(&mut map, "target_kind")?),
        target_id: CapsuleId(take_fixed_bytes::<16>(&mut map, "target_id")?),
    })
}

fn parse_storage_class(value: CapsuleValue) -> CapsuleResult<StorageClass> {
    let mut map = into_object(value, "storage_class")?;
    reject_unknown(&map, &["name", "type"])?;
    let name = parse_optional(take(&mut map, "name")?, |value| match value {
        CapsuleValue::Text(value) => Ok(value),
        _ => Err(invalid("storage class name must be text")),
    })?;
    match (take_text(&mut map, "type")?.as_str(), name) {
        ("core", None) => Ok(StorageClass::Core),
        ("guest_data", None) => Ok(StorageClass::GuestData),
        ("telemetry", None) => Ok(StorageClass::Telemetry),
        ("audit", None) => Ok(StorageClass::Audit),
        ("finance", None) => Ok(StorageClass::Finance),
        ("outbox", None) => Ok(StorageClass::Outbox),
        ("realtime", None) => Ok(StorageClass::Realtime),
        ("module", Some(name)) => Ok(StorageClass::Module(name)),
        _ => Err(invalid("invalid storage class")),
    }
}

fn parse_owner(value: CapsuleValue) -> CapsuleResult<OwnerScope> {
    let mut map = into_object(value, "owner_scope")?;
    reject_unknown(&map, &["id", "type"])?;
    let kind = take_text(&mut map, "type")?;
    let id = take(&mut map, "id")?;
    match (kind.as_str(), id) {
        ("global", CapsuleValue::Null) => Ok(OwnerScope::Global),
        ("node", CapsuleValue::Bytes(value)) => Ok(OwnerScope::Node(
            value
                .try_into()
                .map_err(|_| invalid("node owner ID length"))?,
        )),
        ("creature", CapsuleValue::Bytes(value)) => Ok(OwnerScope::Creature(
            value
                .try_into()
                .map_err(|_| invalid("creature owner ID length"))?,
        )),
        ("module", CapsuleValue::Text(value)) => Ok(OwnerScope::Module(value)),
        _ => Err(invalid("invalid owner scope")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unsealed() -> CapsuleEnvelope {
        CapsuleEnvelope {
            encoding_version: ENCODING_VERSION,
            id: CapsuleId([1; 16]),
            kind: CapsuleKind("core.program".to_owned()),
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Creature([2; 16]),
            schema_version: 1,
            revision: 1,
            created_at_micros: 1_700_000_000_000_000,
            updated_at_micros: 1_700_000_000_000_000,
            previous_integrity: None,
            integrity_hash: CapsuleDigest {
                algorithm: DIGEST_ALGORITHM.to_owned(),
                bytes: vec![0; 32],
            },
            tombstone: false,
            relationships: vec![CapsuleRelationship {
                name: "creature".to_owned(),
                target_kind: CapsuleKind("core.creature".to_owned()),
                target_id: CapsuleId([2; 16]),
            }],
            body: Some(CapsuleValue::Object(BTreeMap::from([
                ("name".to_owned(), CapsuleValue::Text("demo".to_owned())),
                ("weight".to_owned(), CapsuleValue::Float(1.5)),
            ]))),
        }
    }

    #[test]
    fn deterministic_capsule_round_trip_and_integrity() {
        let capsule = unsealed().seal().unwrap();
        let bytes = capsule.canonical_bytes().unwrap();
        assert_eq!(
            CapsuleEnvelope::from_canonical_bytes(&bytes).unwrap(),
            capsule
        );
        assert_eq!(capsule.integrity_hash.bytes.len(), 32);
        assert!(bytes.windows(3).any(|window| window == [0xf9, 0x3e, 0x00]));
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/capsule/fixtures/canonical-v1.json"
        )))
        .unwrap();
        let as_hex = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        assert_eq!(fixture["canonical_hex"], as_hex(&bytes));
        assert_eq!(
            fixture["integrity_hex"],
            as_hex(&capsule.integrity_hash.bytes)
        );

        let mut changed = capsule.clone();
        changed.updated_at_micros += 1;
        assert!(changed.verify().is_err());

        let mut arbitrary_placeholder = unsealed();
        arbitrary_placeholder.integrity_hash = CapsuleDigest {
            algorithm: "not-a-digest".to_owned(),
            bytes: Vec::new(),
        };
        assert!(arbitrary_placeholder.seal().unwrap().verify().is_ok());
    }

    #[test]
    fn noncanonical_and_invalid_revision_forms_fail_closed() {
        assert!(decode_canonical_value(&[0x18, 0x01]).is_err());
        assert!(decode_canonical_value(&[0x9f, 0xff]).is_err());
        assert!(encode_value(&CapsuleValue::Float(f64::NAN)).is_err());
        let mut tombstone = unsealed();
        tombstone.tombstone = true;
        assert!(tombstone.seal().is_err());
        let mut revision = unsealed();
        revision.revision = 2;
        assert!(revision.seal().is_err());

        let fixtures: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/capsule/fixtures/invalid-cbor-v1.json"
        )))
        .unwrap();
        for fixture in fixtures.as_array().unwrap() {
            let source = fixture["hex"].as_str().unwrap().as_bytes();
            let (pairs, remainder) = source.as_chunks::<2>();
            assert!(remainder.is_empty());
            let bytes = pairs
                .iter()
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect::<Vec<_>>();
            assert!(
                decode_canonical_value(&bytes).is_err(),
                "accepted invalid vector {}",
                fixture["name"]
            );
        }
    }

    #[test]
    fn query_rejects_unknown_unsearchable_and_unbounded_fields() {
        let definition = CapsuleDefinition {
            kind: CapsuleKind("core.program".to_owned()),
            schema_version: 1,
            storage_class: StorageClass::Core,
            consistency: ConsistencyProfile::Serializable,
            required_fields: BTreeSet::from(["name".to_owned()]),
            fields: BTreeMap::from([
                (
                    "name".to_owned(),
                    FieldDefinition {
                        field_type: FieldType::Text,
                        searchable: true,
                        sortable: true,
                        nullable: false,
                    },
                ),
                (
                    "secret".to_owned(),
                    FieldDefinition {
                        field_type: FieldType::Text,
                        searchable: false,
                        sortable: false,
                        nullable: true,
                    },
                ),
            ]),
            indexes: Vec::new(),
            relationships: BTreeMap::new(),
        };
        let query = CapsuleQuery {
            kind: definition.kind.clone(),
            predicate: Some(QueryPredicate::Compare {
                field: "name".to_owned(),
                operator: ComparisonOperator::Equal,
                value: CapsuleValue::Text("demo".to_owned()),
            }),
            projection: BTreeSet::from(["name".to_owned()]),
            sort: vec![QuerySort {
                field: "name".to_owned(),
                direction: SortDirection::Ascending,
            }],
            aggregates: vec![QueryAggregate::Count {
                alias: "total".to_owned(),
            }],
            traversals: Vec::new(),
            limit: 100,
            cursor: None,
        };
        assert!(query.validate(&definition).is_ok());
        let mut invalid = query.clone();
        invalid.limit = MAX_QUERY_LIMIT + 1;
        assert!(invalid.validate(&definition).is_err());
        let mut invalid = query;
        invalid.predicate = Some(QueryPredicate::Compare {
            field: "secret".to_owned(),
            operator: ComparisonOperator::Equal,
            value: CapsuleValue::Text("x".to_owned()),
        });
        assert!(invalid.validate(&definition).is_err());

        let invalid_sum = CapsuleQuery {
            kind: definition.kind.clone(),
            predicate: None,
            projection: BTreeSet::new(),
            sort: Vec::new(),
            aggregates: vec![QueryAggregate::Sum {
                field: "name".to_owned(),
                alias: "invalid_total".to_owned(),
            }],
            traversals: Vec::new(),
            limit: 1,
            cursor: None,
        };
        assert!(invalid_sum.validate(&definition).is_err());
    }

    #[test]
    fn relationship_projection_is_validated_against_the_target_definition() {
        let target_kind = CapsuleKind("core.creature".to_owned());
        let source_kind = CapsuleKind("core.program".to_owned());
        let target = CapsuleDefinition {
            kind: target_kind.clone(),
            schema_version: 1,
            storage_class: StorageClass::Core,
            consistency: ConsistencyProfile::Serializable,
            required_fields: BTreeSet::from(["name".to_owned()]),
            fields: BTreeMap::from([(
                "name".to_owned(),
                FieldDefinition {
                    field_type: FieldType::Text,
                    searchable: true,
                    sortable: true,
                    nullable: false,
                },
            )]),
            indexes: Vec::new(),
            relationships: BTreeMap::new(),
        };
        let source = CapsuleDefinition {
            kind: source_kind.clone(),
            schema_version: 1,
            storage_class: StorageClass::Core,
            consistency: ConsistencyProfile::Serializable,
            required_fields: BTreeSet::from(["name".to_owned()]),
            fields: target.fields.clone(),
            indexes: Vec::new(),
            relationships: BTreeMap::from([(
                "creature".to_owned(),
                RelationshipDefinition {
                    target_kind: target_kind.clone(),
                    required: true,
                },
            )]),
        };
        let mut query = CapsuleQuery {
            kind: source_kind.clone(),
            predicate: None,
            projection: BTreeSet::new(),
            sort: Vec::new(),
            aggregates: Vec::new(),
            traversals: vec![RelationshipTraversal {
                relationship: "creature".to_owned(),
                target_kind: target_kind.clone(),
                projection: BTreeSet::from(["name".to_owned()]),
                limit: 1,
            }],
            limit: 1,
            cursor: None,
        };
        let definitions = BTreeMap::from([(source_kind, source), (target_kind, target)]);
        assert!(query.validate_with_registry(&definitions).is_ok());
        query.traversals[0]
            .projection
            .insert("undeclared".to_owned());
        assert!(query.validate_with_registry(&definitions).is_err());
    }

    #[test]
    fn capability_negotiation_never_silently_weakens_consistency() {
        assert_eq!(
            serde_json::to_string(&StorageCapability::TransactionsSingleCapsule).unwrap(),
            "\"transactions.single_capsule\""
        );
        assert_eq!(
            StorageCapability::TransactionsSingleCapsule.wire_name(),
            "transactions.single_capsule"
        );
        let definition = CapsuleDefinition {
            kind: CapsuleKind("finance.ledger_entry".to_owned()),
            schema_version: 1,
            storage_class: StorageClass::Finance,
            consistency: ConsistencyProfile::Serializable,
            required_fields: BTreeSet::from(["entry_id".to_owned()]),
            fields: BTreeMap::from([(
                "entry_id".to_owned(),
                FieldDefinition {
                    field_type: FieldType::CapsuleId,
                    searchable: true,
                    sortable: true,
                    nullable: false,
                },
            )]),
            indexes: vec![IndexDefinition {
                name: "entry_id".to_owned(),
                fields: vec!["entry_id".to_owned()],
                unique: true,
            }],
            relationships: BTreeMap::new(),
        };
        let provider = ProviderCapabilities {
            provider_id: "eventual-only".to_owned(),
            capabilities: BTreeSet::from([
                StorageCapability::TransactionsSingleCapsule,
                StorageCapability::ConsistencyEventual,
            ]),
            max_query_limit: 100,
            max_transaction_capsules: 1,
        };
        let report = negotiate_capabilities(&definition, &provider).unwrap();
        assert!(!report.compatible);
        assert!(
            report
                .missing
                .contains(&StorageCapability::ConsistencyLinearizable)
        );
        assert!(report.missing.contains(&StorageCapability::IndexesUnique));
    }
}
