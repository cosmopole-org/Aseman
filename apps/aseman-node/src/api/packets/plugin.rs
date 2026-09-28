//! Request and response payloads for the `plugin` action namespace.

use serde::{Deserialize, Serialize};

use crate::api::model::Creature;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[expect(
    dead_code,
    reason = "RL-004: characterized legacy action surface (A008) kept until its deletion gate"
)]
pub struct AssignOutput {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[expect(
    dead_code,
    reason = "RL-004: characterized legacy action surface (A008) kept until its deletion gate"
)]
pub struct CreateOutput {
    #[serde(default)]
    pub user: Creature,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlugInput {}
