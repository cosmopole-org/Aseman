//! Request, response, and federation-broadcast payloads for the `stores`
//! action namespace.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::input::IInput;
use crate::shell::api::model::{Creature, Store};

fn is_false(b: &bool) -> bool {
    !*b
}

fn store_is_empty(s: &Store) -> bool {
    s.id.is_empty() && s.tag.is_empty() && s.parent_id.is_empty()
}

// ---- Inputs ----------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SignalInput {
    #[serde(rename = "type", default)]
    pub typ: String,
    #[serde(rename = "storeId", default)]
    pub store_id: String,
    #[serde(rename = "userId", default)]
    pub user_id: String,
    #[serde(default)]
    pub data: String,
    /// Sender-supplied labels persisted with the packet, filtered on later by
    /// `stores/history`. Rejected outright when malformed.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Ephemeral: fan out live, never persist — regardless of the store's
    /// `persHist`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub temp: bool,
    /// Federation origin. Empty means "this node"; a foreign origin routes the
    /// whole action to that node, which serves it against its own log.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin: String,
}

impl IInput for SignalInput {
    fn get_store_id(&self) -> String {
        self.store_id.clone()
    }
    fn origin(&self) -> String {
        self.origin.clone()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A tag-filtered read over one store's persisted signals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HistoryInput {
    #[serde(rename = "storeId", default)]
    pub store_id: String,
    /// Every one of these tags must be present on a packet for it to match.
    #[serde(rename = "tagsAll", default)]
    pub tags_all: Vec<String>,
    /// At least one of these must be present (empty = no constraint).
    #[serde(rename = "tagsAny", default)]
    pub tags_any: Vec<String>,
    /// Page backwards from this `time` (exclusive). 0 = newest.
    #[serde(rename = "beforeTime", default)]
    pub before_time: i64,
    /// Only packets newer than this `time` (exclusive). 0 = unbounded.
    #[serde(rename = "afterTime", default)]
    pub after_time: i64,
    #[serde(default)]
    pub count: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin: String,
}

impl IInput for HistoryInput {
    fn get_store_id(&self) -> String {
        self.store_id.clone()
    }
    fn origin(&self) -> String {
        self.origin.clone()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Grant (or revoke) one member's permissions within a store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetAccessInput {
    #[serde(rename = "storeId", default)]
    pub store_id: String,
    /// The member being granted — a person or a machine/program id.
    #[serde(rename = "memberId", default)]
    pub member_id: String,
    /// Permission flag names (`read`, `signal`, `manage`). An empty list
    /// revokes every permission while leaving membership intact.
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin: String,
}

impl IInput for SetAccessInput {
    fn get_store_id(&self) -> String {
        self.store_id.clone()
    }
    fn origin(&self) -> String {
        self.origin.clone()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Read one member's permissions (defaults to the caller).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetAccessInput {
    #[serde(rename = "storeId", default)]
    pub store_id: String,
    #[serde(rename = "memberId", default)]
    pub member_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin: String,
}

impl IInput for GetAccessInput {
    fn get_store_id(&self) -> String {
        self.store_id.clone()
    }
    fn origin(&self) -> String {
        self.origin.clone()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JoinInput {
    #[serde(rename = "storeId", default)]
    pub store_id: String,
}

// ---- Outputs ---------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdminPoiint {
    #[serde(default)]
    pub store: Store,
    #[serde(default)]
    pub admin: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateOutput {
    #[serde(default)]
    pub store: AdminPoiint,
}

// ---- Updates ---------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Send {
    #[serde(default)]
    pub user: Creature,
    #[serde(default, skip_serializing_if = "store_is_empty")]
    pub store: Store,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub data: String,
    #[serde(rename = "isTemp", default, skip_serializing_if = "is_false")]
    pub is_temp: bool,
    #[serde(rename = "entityId", default, skip_serializing_if = "String::is_empty")]
    pub entity_id: String,
    /// Correlation id carried across a proxy-entity round trip: the requester
    /// (or the proxy itself) stamps it on the forwarded signal, the target
    /// echoes it on its response signal, and the node uses it to route the
    /// response back through the proxy entity to the original sender.
    #[serde(
        rename = "correlationId",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub correlation_id: String,
    /// Id of the persisted log row this signal produced, when it was persisted.
    /// A live listener keys on it to recognise the same packet when it later
    /// replays out of `stores/history`, instead of rendering it twice.
    #[serde(rename = "signalId", default, skip_serializing_if = "String::is_empty")]
    pub signal_id: String,
    /// The sender's tags, carried on the live fan-out so a listener can apply
    /// exactly the filter it would apply to history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Log time of the persisted row (ms).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub time: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Update {
    #[serde(default)]
    pub store: Store,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Delete {
    #[serde(default)]
    pub store: Store,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddMember {
    #[serde(rename = "storeId", default)]
    pub store_id: String,
    #[serde(default)]
    pub user: Creature,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateMember {
    #[serde(rename = "storeId", default)]
    pub store_id: String,
    #[serde(default)]
    pub user: Creature,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Join {
    #[serde(rename = "storeId", default)]
    pub store_id: String,
    #[serde(default)]
    pub user: Creature,
}
