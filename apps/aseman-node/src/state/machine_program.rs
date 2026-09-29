//! The legacy wire shape of a program (persisted as `core.program`, ADR 0036).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Program {
    // The program's own identity.
    #[serde(rename = "id", default)]
    pub id: String,
    // Link to the owning Machine object (machines = former "apps"). This is the
    // canonical owner relationship; there is no separate app_id pointer.
    #[serde(rename = "machineId", default)]
    pub machine_id: String,
    #[serde(default)]
    pub runtime: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub comment: String,
}
