//! Translation of `src/bots/sampleBot/{inputs,outputs}`.
//!
//! Both Go packages defined the same four types verbatim; we keep the
//! single Rust copy here.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HelloInput {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HelloOutput {
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ByeInput {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ByeOutput {
    #[serde(default)]
    pub message: String,
}
