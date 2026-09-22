//! Guest API values (A405, ADR 0001, ADR 0021): the legacy key/value operations a
//! workload may perform on its own creature's guest database, and their limits. The
//! caller never names the creature, database, or role; the gateway derives them from
//! the authenticated workload.

use serde::{Deserialize, Serialize};

/// The registered action every guest data operation is authorized as (A402).
pub const GUEST_DATA_ACTION: &str = "guest_data.access";
/// Longest guest key, in bytes.
pub const MAX_GUEST_KEY_BYTES: usize = 1_024;
/// Largest guest value, in bytes.
pub const MAX_GUEST_VALUE_BYTES: usize = 1 << 20;
/// Most pairs one listing returns.
pub const MAX_GUEST_LIST: u32 = 1_000;

/// The two legacy key spaces of the reserved `_aseman_legacy_kv` table (ADR 0021).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyKvNamespace {
    /// Runtime `dbOp` calls.
    #[serde(rename = "dbop")]
    DbOp,
    /// Host-function `dbOp` calls.
    AppletDb,
    /// Confined guest documents (ADR 0028): one row per legacy JSON record, keyed
    /// `{key}::{path}`.
    Json,
}

impl LegacyKvNamespace {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DbOp => "dbop",
            Self::AppletDb => "applet_db",
            Self::Json => "json",
        }
    }
}

/// One guest key/value operation. From cutover, `Delete` really deletes and `List`
/// returns committed pairs (ADR 0021 documents both as behavior changes).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum GuestKvOperation {
    Get {
        namespace: LegacyKvNamespace,
        key: String,
    },
    Put {
        namespace: LegacyKvNamespace,
        key: String,
        value: String,
    },
    Delete {
        namespace: LegacyKvNamespace,
        key: String,
    },
    List {
        namespace: LegacyKvNamespace,
        prefix: String,
        limit: u32,
    },
    /// Legacy `putJson`: `data` is a JSON object's text, indexed at `{key}::{path}` in
    /// the `json` namespace exactly as the legacy JSON store did (ADR 0028).
    PutJson {
        key: String,
        path: String,
        data: String,
        merge: bool,
    },
    /// Legacy `getJson`: the object stored at `{key}::{path}`, or `{}`.
    GetJson { key: String, path: String },
    /// Legacy `delKey`: the whole document when `path` is empty, else the subtree at
    /// `path`.
    DeleteJson { key: String, path: String },
    /// Legacy `getByPrefix`: the document record keys (`{key}::{path}`) with a prefix.
    ListJson { prefix: String, limit: u32 },
}

impl GuestKvOperation {
    /// Whether the operation is within the guest API limits.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let key_ok = |key: &str| !key.is_empty() && key.len() <= MAX_GUEST_KEY_BYTES;
        match self {
            Self::Get { key, .. } | Self::Delete { key, .. } => key_ok(key),
            Self::Put { key, value, .. } => key_ok(key) && value.len() <= MAX_GUEST_VALUE_BYTES,
            Self::List { prefix, limit, .. } | Self::ListJson { prefix, limit } => {
                prefix.len() <= MAX_GUEST_KEY_BYTES && (1..=MAX_GUEST_LIST).contains(limit)
            }
            Self::PutJson {
                key, path, data, ..
            } => {
                key_ok(key)
                    && path.len() <= MAX_GUEST_KEY_BYTES
                    && data.len() <= MAX_GUEST_VALUE_BYTES
            }
            Self::GetJson { key, path } | Self::DeleteJson { key, path } => {
                key_ok(key) && path.len() <= MAX_GUEST_KEY_BYTES
            }
        }
    }
}

/// The result of a guest key/value operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum GuestKvOutcome {
    Value {
        value: Option<String>,
    },
    Written,
    Deleted {
        existed: bool,
    },
    Listed {
        pairs: Vec<(String, String)>,
    },
    /// A JSON object's text.
    Document {
        data: String,
    },
    Keys {
        keys: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_stay_within_the_guest_limits() {
        let get = |key: &str| GuestKvOperation::Get {
            namespace: LegacyKvNamespace::DbOp,
            key: key.to_owned(),
        };
        assert!(get("profile").is_valid());
        assert!(!get("").is_valid());
        assert!(!get(&"k".repeat(MAX_GUEST_KEY_BYTES + 1)).is_valid());
        let list = |limit| GuestKvOperation::List {
            namespace: LegacyKvNamespace::AppletDb,
            prefix: String::new(),
            limit,
        };
        assert!(list(10).is_valid());
        assert!(!list(0).is_valid());
        assert!(!list(MAX_GUEST_LIST + 1).is_valid());
        assert!(
            !GuestKvOperation::Put {
                namespace: LegacyKvNamespace::DbOp,
                key: "k".to_owned(),
                value: "v".repeat(MAX_GUEST_VALUE_BYTES + 1),
            }
            .is_valid()
        );
    }
}
