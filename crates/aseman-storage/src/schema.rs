//! The model catalog: every persisted family, from the provider-neutral schemas in
//! `contracts/capsule/kinds` (ADR 0036).

use crate::error::{StorageError, StorageResult};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

const CORE_SCHEMAS: &str =
    include_str!("../../../contracts/capsule/kinds/core-logical-schemas.json");
const CLASS_SCHEMAS: &str =
    include_str!("../../../contracts/capsule/kinds/storage-class-logical-schemas.json");
const CORE_REGISTRY: &str = include_str!("../../../contracts/capsule/kinds/core-registry.json");
const CLASS_REGISTRY: &str =
    include_str!("../../../contracts/capsule/kinds/storage-class-registry.json");

/// A field's logical type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldType {
    Text,
    Integer,
    Float,
    Bool,
    Bytes,
    TimestampMicros,
    /// A 16-byte id that is not a relation.
    CapsuleId,
    /// A structured document (JSON object).
    Document,
}

impl FieldType {
    fn parse(name: &str) -> StorageResult<Self> {
        Ok(match name {
            "text" => Self::Text,
            "integer" => Self::Integer,
            "float" => Self::Float,
            "bool" => Self::Bool,
            "bytes" => Self::Bytes,
            "timestamp_micros" => Self::TimestampMicros,
            "capsule_id" => Self::CapsuleId,
            "document" => Self::Document,
            other => return Err(StorageError::invalid(format!("unknown field type {other}"))),
        })
    }

    /// The contract spelling.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Bool => "bool",
            Self::Bytes => "bytes",
            Self::TimestampMicros => "timestamp_micros",
            Self::CapsuleId => "capsule_id",
            Self::Document => "document",
        }
    }
}

/// Who owns a model's records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerScope {
    Global,
    Node,
    Creature,
}

/// One model.
#[derive(Clone, Debug)]
pub struct Model {
    /// The kind, e.g. `core.store`.
    pub name: String,
    /// The capsule storage class spelling (`core`, `finance`, ...).
    pub storage_class: String,
    pub owner_scope: OwnerScope,
    /// The relation whose target owns a record of a creature-scoped model.
    pub owner: Option<String>,
    /// For a keyed model: the family its ids derive from (`Id::for_key`). Its natural
    /// key is the unique text field `key`.
    pub key_family: Option<String>,
    pub fields: BTreeMap<String, FieldType>,
    pub required: BTreeSet<String>,
    pub unique: Vec<Vec<String>>,
    pub range_indexes: Vec<Vec<String>>,
    /// Relation name -> target model.
    pub relations: BTreeMap<String, String>,
    /// Append-only models accept only creates.
    pub append_only: bool,
}

impl Model {
    /// Whether `name` is a field or a relation of this model.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.fields.contains_key(name) || self.relations.contains_key(name)
    }

    /// Every field or relation some index covers, in index order.
    #[must_use]
    pub fn indexed(&self) -> BTreeSet<&str> {
        self.unique
            .iter()
            .chain(&self.range_indexes)
            .flatten()
            .map(String::as_str)
            .chain(self.relations.keys().map(String::as_str))
            .collect()
    }
}

#[derive(Deserialize)]
struct Logical {
    definitions: Vec<LogicalModel>,
}

#[derive(Deserialize)]
struct LogicalModel {
    kind: String,
    fields: BTreeMap<String, String>,
    #[serde(default)]
    required: Vec<String>,
    #[serde(default)]
    unique_indexes: Vec<Vec<String>>,
    #[serde(default)]
    range_indexes: Vec<Vec<String>>,
    #[serde(default)]
    relationships: BTreeMap<String, String>,
    #[serde(default)]
    mutation_policy: Option<String>,
    #[serde(default)]
    key_family: Option<String>,
    #[serde(default)]
    owner: Option<String>,
}

#[derive(Deserialize)]
struct Registry {
    kinds: Vec<RegistryKind>,
}

#[derive(Deserialize)]
struct RegistryKind {
    kind: String,
    storage_class: String,
    owner_scope: String,
}

/// Every model, by name.
#[derive(Clone, Debug)]
pub struct Schema {
    models: BTreeMap<String, Model>,
}

impl Schema {
    /// The compiled-in catalog.
    pub fn catalog() -> StorageResult<Arc<Schema>> {
        static CATALOG: OnceLock<Result<Arc<Schema>, String>> = OnceLock::new();
        CATALOG
            .get_or_init(|| {
                Schema::parse(
                    &[CORE_SCHEMAS, CLASS_SCHEMAS],
                    &[CORE_REGISTRY, CLASS_REGISTRY],
                )
                .map(Arc::new)
                .map_err(|error| error.to_string())
            })
            .clone()
            .map_err(StorageError::invalid)
    }

    /// Parse logical schemas and registries.
    pub fn parse(logical: &[&str], registries: &[&str]) -> StorageResult<Schema> {
        let mut placement = BTreeMap::new();
        for source in registries {
            let registry: Registry = serde_json::from_str(source)
                .map_err(|error| StorageError::invalid(format!("model registry: {error}")))?;
            for kind in registry.kinds {
                placement.insert(kind.kind, (kind.storage_class, kind.owner_scope));
            }
        }
        let mut models = BTreeMap::new();
        for source in logical {
            let logical: Logical = serde_json::from_str(source)
                .map_err(|error| StorageError::invalid(format!("model schema: {error}")))?;
            for definition in logical.definitions {
                let (storage_class, owner_scope) =
                    placement.get(&definition.kind).cloned().ok_or_else(|| {
                        StorageError::invalid(format!("{} has no registry row", definition.kind))
                    })?;
                let owner_scope = match owner_scope.as_str() {
                    "global" => OwnerScope::Global,
                    "node" => OwnerScope::Node,
                    "creature" => OwnerScope::Creature,
                    other => {
                        return Err(StorageError::invalid(format!("owner scope {other}")));
                    }
                };
                let fields = definition
                    .fields
                    .iter()
                    .map(|(name, kind)| Ok((name.clone(), FieldType::parse(kind)?)))
                    .collect::<StorageResult<BTreeMap<_, _>>>()?;
                let owner = definition.owner.or_else(|| {
                    (owner_scope == OwnerScope::Creature
                        && definition.relationships.contains_key("creature"))
                    .then(|| "creature".to_owned())
                });
                let model = Model {
                    name: definition.kind.clone(),
                    storage_class,
                    owner_scope,
                    owner,
                    key_family: definition.key_family,
                    fields,
                    required: definition.required.into_iter().collect(),
                    unique: definition.unique_indexes,
                    range_indexes: definition.range_indexes,
                    relations: definition.relationships,
                    append_only: definition.mutation_policy.as_deref() == Some("append_only"),
                };
                model.validate()?;
                models.insert(definition.kind, model);
            }
        }
        for model in models.values() {
            for target in model.relations.values() {
                if !models.contains_key(target) {
                    return Err(StorageError::invalid(format!(
                        "{} relates to unknown model {target}",
                        model.name
                    )));
                }
            }
        }
        Ok(Schema { models })
    }

    pub fn model(&self, name: &str) -> StorageResult<&Model> {
        self.models
            .get(name)
            .ok_or_else(|| StorageError::invalid(format!("unknown model {name}")))
    }

    pub fn models(&self) -> impl Iterator<Item = &Model> {
        self.models.values()
    }
}

impl Model {
    fn validate(&self) -> StorageResult<()> {
        let invalid = |what: &str| StorageError::invalid(format!("{}: {what}", self.name));
        if self.fields.keys().any(|field| self.relations.contains_key(field)) {
            return Err(invalid("a name is both a field and a relation"));
        }
        if self.required.iter().any(|field| !self.fields.contains_key(field)) {
            return Err(invalid("a required field is undeclared"));
        }
        for index in self.unique.iter().chain(&self.range_indexes) {
            if index.is_empty() || index.iter().any(|field| !self.has(field)) {
                return Err(invalid("an index names an undeclared field"));
            }
        }
        if let Some(owner) = &self.owner
            && !self.relations.contains_key(owner)
        {
            return Err(invalid("the owner is not a relation"));
        }
        if self.key_family.is_some()
            && (self.fields.get("key") != Some(&FieldType::Text)
                || !self.unique.iter().any(|index| index == &["key"]))
        {
            return Err(invalid("a keyed model needs a unique text field `key`"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_loads_every_registered_model() {
        let schema = Schema::catalog().unwrap();
        let store = schema.model("core.store").unwrap();
        assert_eq!(store.relations["creature"], "core.creature");
        assert_eq!(store.owner.as_deref(), Some("creature"));
        assert!(schema.model("finance.ledger_entry").unwrap().append_only);
        assert!(schema.model("core.nothing").is_err());
    }
}
