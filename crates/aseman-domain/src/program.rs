//! Program values shared by the program use cases and their adapters.
//!
//! Program and machine identities are legacy string identities; capsule adapters map
//! them to canonical IDs. A machine may own several programs.

use serde::{Deserialize, Serialize};

/// One program as legacy exposes it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProgramRecord {
    pub id: String,
    /// The machine creature that owns the program.
    pub machine_id: String,
    pub runtime: String,
    pub path: String,
    pub comment: String,
}

/// The entity an alarm without one replays, as legacy did.
pub const DEFAULT_ALARM_ENTITY: &str = "main";

/// A program's pending wake-up (legacy `vmAlarm*`). A program has at most one.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProgramAlarm {
    /// The store the program runs in when it wakes.
    pub store_id: String,
    /// Unix milliseconds, as legacy stored it.
    pub fire_at_millis: i64,
    pub data: String,
    /// The entity to run; legacy alarms without one run [`DEFAULT_ALARM_ENTITY`].
    pub entity: String,
}

/// A VM resource store (legacy `Json::VmResourceStore`, target
/// `core.vm_resource_store`): a named document owned by a machine.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmResourceStore {
    pub id: String,
    pub name: String,
    /// The owning machine creature, or a program whose machine owns it.
    pub machine_id: String,
    /// The metadata document as compact JSON object text.
    pub metadata: String,
}
