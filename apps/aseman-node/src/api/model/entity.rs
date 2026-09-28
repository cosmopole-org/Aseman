//! The legacy wire shape of a program entity (persisted as `core.entity`, ADR 0036).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entity {
    #[serde(rename = "programId", default)]
    pub program_id: String,
    #[serde(rename = "entityId", default)]
    pub entity_id: String,
    #[serde(rename = "entityType", default)]
    pub entity_type: String,
    #[serde(rename = "imageName", default)]
    pub image_name: String,
}
