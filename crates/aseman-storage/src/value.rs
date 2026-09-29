//! Record values.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// A record id: the 16 bytes of the underlying capsule id.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct Id(pub [u8; 16]);

impl Id {
    /// A fresh time-ordered id (UUIDv7).
    #[must_use]
    pub fn generate() -> Self {
        Self(*uuid::Uuid::now_v7().as_bytes())
    }

    /// The id of a keyed model's record: the same derivation A308 used, so migrated
    /// records keep their ids.
    #[must_use]
    pub fn for_key(family: &str, key: &str) -> Self {
        Self(
            aseman_contracts::legacy_realtime::deterministic_legacy_capsule_id(
                family,
                key.as_bytes(),
            ),
        )
    }
}

impl fmt::Display for Id {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", uuid::Uuid::from_bytes(self.0))
    }
}

/// One field value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    /// A document field.
    Json(serde_json::Value),
    /// A relation, or a field of type `capsule_id`.
    Id(Id),
}

impl Value {
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_id(&self) -> Option<Id> {
        match self {
            Self::Id(id) => Some(*id),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_json(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Json(value) => Some(value),
            _ => None,
        }
    }
}

macro_rules! from {
    ($($type:ty => $variant:ident),* $(,)?) => {
        $(impl From<$type> for Value {
            fn from(value: $type) -> Self {
                Self::$variant(value.into())
            }
        })*
    };
}

from!(bool => Bool, i64 => Int, i32 => Int, u32 => Int, f64 => Float, String => Text,
      &str => Text, Vec<u8> => Bytes, serde_json::Value => Json, Id => Id);

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Into::into)
    }
}

/// A record's fields and relations, by name.
pub type Data = BTreeMap<String, Value>;

/// A stored record.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: Id,
    pub revision: u64,
    pub created_at_micros: i64,
    pub updated_at_micros: i64,
    pub data: Data,
}

impl Row {
    /// A field, or `Null` when absent.
    #[must_use]
    pub fn get(&self, field: &str) -> &Value {
        self.data.get(field).unwrap_or(&Value::Null)
    }

    #[must_use]
    pub fn text(&self, field: &str) -> Option<&str> {
        self.get(field).as_text()
    }

    #[must_use]
    pub fn int(&self, field: &str) -> Option<i64> {
        self.get(field).as_int()
    }

    #[must_use]
    pub fn id_of(&self, field: &str) -> Option<Id> {
        self.get(field).as_id()
    }
}
