//! Portable guest database bindings and bounded multi-table schema mutations.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const GUEST_SCHEMA_VERSION: u16 = 1;
pub const MAX_GUEST_TABLES: usize = 128;
pub const MAX_GUEST_COLUMNS: usize = 128;
pub const MAX_GUEST_INDEXES: usize = 32;
pub const MAX_GUEST_INDEX_COLUMNS: usize = 16;
pub const RESERVED_METADATA_COLUMNS: &[&str] = &[
    "_aseman_id",
    "_aseman_revision",
    "_aseman_created_at_micros",
    "_aseman_updated_at_micros",
    "_aseman_integrity",
    "_aseman_tombstone",
];

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum GuestContractError {
    #[error("invalid guest schema: {0}")]
    Invalid(String),
}

pub type GuestContractResult<T> = Result<T, GuestContractError>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestBindingStatus {
    Provisioning,
    Disabled,
    Active,
    Draining,
    Retained,
    Retired,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestDatabaseBinding {
    pub creature_id: [u8; 16],
    pub provider_id: String,
    pub database_name: String,
    pub role_name: String,
    pub generation: u64,
    pub schema_catalog_revision: u64,
    pub status: GuestBindingStatus,
}

/// A catalog compare-and-swap around one portable schema mutation.
///
/// Routing is deliberately absent: the server supplies the authenticated creature
/// binding independently, while the caller can only name the revision it observed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestSchemaCommand {
    pub expected_catalog_revision: u64,
    pub mutation: GuestSchemaMutation,
}

impl GuestSchemaCommand {
    pub fn validate(&self) -> GuestContractResult<()> {
        self.mutation.validate()
    }
}

impl GuestDatabaseBinding {
    pub fn validate(&self) -> GuestContractResult<()> {
        validate_provider_token(&self.provider_id)?;
        validate_identifier("database_name", &self.database_name)?;
        validate_identifier("role_name", &self.role_name)?;
        if self.generation == 0 {
            return Err(invalid("binding generation starts at one"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestFieldType {
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
pub struct GuestColumnDefinition {
    pub field_type: GuestFieldType,
    pub required: bool,
    pub default: Option<GuestDefaultValue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum GuestDefaultValue {
    Bool(bool),
    Integer(i64),
    Float(String),
    Bytes(Vec<u8>),
    Text(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestIndexDefinition {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestForeignKeyDefinition {
    pub name: String,
    pub columns: Vec<String>,
    pub target_table: String,
    pub target_columns: Vec<String>,
    pub on_delete: GuestDeleteAction,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestDeleteAction {
    Restrict,
    Cascade,
    SetNull,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestTableDefinition {
    pub name: String,
    pub columns: BTreeMap<String, GuestColumnDefinition>,
    pub primary_key: Vec<String>,
    pub indexes: Vec<GuestIndexDefinition>,
    pub foreign_keys: Vec<GuestForeignKeyDefinition>,
}

impl GuestTableDefinition {
    pub fn validate(&self) -> GuestContractResult<()> {
        validate_user_identifier("table", &self.name)?;
        if self.columns.is_empty() || self.columns.len() > MAX_GUEST_COLUMNS {
            return Err(invalid("guest table column count is outside bounds"));
        }
        for (name, column) in &self.columns {
            validate_user_identifier("column", name)?;
            if let Some(default) = &column.default
                && !default_matches(&column.field_type, default)
            {
                return Err(invalid("column default does not match its field type"));
            }
        }
        validate_column_list("primary key", &self.primary_key, &self.columns, false)?;
        if self.indexes.len() > MAX_GUEST_INDEXES {
            return Err(invalid("guest table index count exceeds the limit"));
        }
        let mut names = BTreeSet::new();
        for index in &self.indexes {
            validate_user_identifier("index", &index.name)?;
            if !names.insert(index.name.as_str()) {
                return Err(invalid("guest index names must be unique"));
            }
            validate_column_list("index", &index.columns, &self.columns, true)?;
        }
        for foreign_key in &self.foreign_keys {
            validate_user_identifier("foreign key", &foreign_key.name)?;
            validate_user_identifier("foreign key target table", &foreign_key.target_table)?;
            validate_column_list("foreign key", &foreign_key.columns, &self.columns, true)?;
            if foreign_key.target_columns.is_empty()
                || foreign_key.target_columns.len() != foreign_key.columns.len()
                || foreign_key.target_columns.len() > MAX_GUEST_INDEX_COLUMNS
            {
                return Err(invalid("foreign key target columns are invalid"));
            }
            for column in &foreign_key.target_columns {
                validate_user_identifier("foreign key target column", column)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "operation", deny_unknown_fields)]
pub enum GuestSchemaMutation {
    CreateTable {
        definition: GuestTableDefinition,
    },
    AddColumn {
        table: String,
        name: String,
        definition: GuestColumnDefinition,
    },
    CreateIndex {
        table: String,
        definition: GuestIndexDefinition,
    },
    DropIndex {
        table: String,
        name: String,
    },
    DropTable {
        table: String,
        destructive_change_approved: bool,
    },
}

impl GuestSchemaMutation {
    pub fn validate(&self) -> GuestContractResult<()> {
        match self {
            Self::CreateTable { definition } => definition.validate(),
            Self::AddColumn {
                table,
                name,
                definition,
            } => {
                validate_user_identifier("table", table)?;
                validate_user_identifier("column", name)?;
                if definition.required && definition.default.is_none() {
                    return Err(invalid("a required added column needs a portable default"));
                }
                if let Some(default) = &definition.default
                    && !default_matches(&definition.field_type, default)
                {
                    return Err(invalid("column default does not match its field type"));
                }
                Ok(())
            }
            Self::CreateIndex { table, definition } => {
                validate_user_identifier("table", table)?;
                validate_user_identifier("index", &definition.name)?;
                if definition.columns.is_empty()
                    || definition.columns.len() > MAX_GUEST_INDEX_COLUMNS
                {
                    return Err(invalid("guest index column count is invalid"));
                }
                for column in &definition.columns {
                    validate_user_identifier("index column", column)?;
                }
                Ok(())
            }
            Self::DropIndex { table, name } => {
                validate_user_identifier("table", table)?;
                validate_user_identifier("index", name)
            }
            Self::DropTable {
                table,
                destructive_change_approved,
            } => {
                validate_user_identifier("table", table)?;
                if !destructive_change_approved {
                    return Err(invalid("drop table requires explicit policy approval"));
                }
                Ok(())
            }
        }
    }
}

fn validate_column_list(
    label: &str,
    columns: &[String],
    declared: &BTreeMap<String, GuestColumnDefinition>,
    required: bool,
) -> GuestContractResult<()> {
    if (required && columns.is_empty()) || columns.len() > MAX_GUEST_INDEX_COLUMNS {
        return Err(invalid(&format!("{label} column count is invalid")));
    }
    let unique = columns.iter().collect::<BTreeSet<_>>();
    if unique.len() != columns.len() || columns.iter().any(|column| !declared.contains_key(column))
    {
        return Err(invalid(&format!(
            "{label} columns must be unique and declared"
        )));
    }
    Ok(())
}

fn default_matches(field_type: &GuestFieldType, value: &GuestDefaultValue) -> bool {
    let type_matches = matches!(
        (field_type, value),
        (GuestFieldType::Bool, GuestDefaultValue::Bool(_))
            | (
                GuestFieldType::Integer | GuestFieldType::TimestampMicros,
                GuestDefaultValue::Integer(_)
            )
            | (GuestFieldType::Bytes, GuestDefaultValue::Bytes(_))
            | (GuestFieldType::Text, GuestDefaultValue::Text(_))
            | (GuestFieldType::CapsuleId, GuestDefaultValue::Bytes(_))
    ) || matches!(
        (field_type, value),
        (GuestFieldType::Float, GuestDefaultValue::Float(value))
            if value.parse::<f64>().is_ok_and(f64::is_finite)
    );
    type_matches
        && (!matches!(field_type, GuestFieldType::CapsuleId)
            || matches!(value, GuestDefaultValue::Bytes(bytes) if bytes.len() == 16))
}

fn validate_user_identifier(label: &str, value: &str) -> GuestContractResult<()> {
    validate_identifier(label, value)?;
    if value.starts_with("_aseman_")
        || value.starts_with("pg_")
        || value == "information_schema"
        || value == "guest_capsules"
        || RESERVED_METADATA_COLUMNS.contains(&value)
    {
        return Err(invalid(&format!("{label} uses a reserved name")));
    }
    Ok(())
}

fn validate_identifier(label: &str, value: &str) -> GuestContractResult<()> {
    if value.is_empty()
        || value.len() > 63
        || !value.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_lowercase()
            } else {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
            }
        })
    {
        return Err(invalid(&format!("{label} is not a safe identifier")));
    }
    Ok(())
}

fn validate_provider_token(value: &str) -> GuestContractResult<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(invalid("provider_id is not a safe token"));
    }
    Ok(())
}

fn invalid(message: &str) -> GuestContractError {
    GuestContractError::Invalid(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> GuestTableDefinition {
        GuestTableDefinition {
            name: "orders".to_owned(),
            columns: BTreeMap::from([
                (
                    "order_id".to_owned(),
                    GuestColumnDefinition {
                        field_type: GuestFieldType::CapsuleId,
                        required: true,
                        default: None,
                    },
                ),
                (
                    "amount".to_owned(),
                    GuestColumnDefinition {
                        field_type: GuestFieldType::Integer,
                        required: true,
                        default: Some(GuestDefaultValue::Integer(0)),
                    },
                ),
            ]),
            primary_key: vec!["order_id".to_owned()],
            indexes: vec![GuestIndexDefinition {
                name: "orders_amount".to_owned(),
                columns: vec!["amount".to_owned()],
                unique: false,
            }],
            foreign_keys: Vec::new(),
        }
    }

    #[test]
    fn multi_table_definitions_are_typed_and_reserved_names_fail_closed() {
        assert!(table().validate().is_ok());
        let mut invalid = table();
        invalid.name = "guest_capsules".to_owned();
        assert!(invalid.validate().is_err());
        let mut invalid = table();
        invalid.columns.insert(
            "_aseman_revision".to_owned(),
            invalid.columns["amount"].clone(),
        );
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn destructive_schema_changes_require_policy_approval() {
        assert!(
            GuestSchemaMutation::DropTable {
                table: "orders".to_owned(),
                destructive_change_approved: false,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn checked_in_multi_table_fixture_parses_and_caller_routing_is_rejected() {
        let mutations: Vec<GuestSchemaMutation> = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/capsule/guest/fixtures/valid-multi-table.json"
        )))
        .unwrap();
        assert_eq!(mutations.len(), 2);
        assert!(mutations.iter().all(|mutation| mutation.validate().is_ok()));
        assert!(
            serde_json::from_str::<GuestSchemaMutation>(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../contracts/capsule/guest/fixtures/invalid-caller-routing.json"
            )))
            .is_err()
        );

        let command = GuestSchemaCommand {
            expected_catalog_revision: 0,
            mutation: mutations[0].clone(),
        };
        let encoded = serde_json::to_value(command).unwrap();
        assert!(encoded.get("database_name").is_none());
        assert!(encoded.get("role_name").is_none());
        assert!(encoded.get("provider_id").is_none());
    }
}
