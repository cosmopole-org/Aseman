//! Request and response payloads for the trivial action namespaces —
//! `empty`, `hello`, `ping`.

use serde::{Deserialize, Serialize};

use crate::models::input::IInput;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmptyInput {}

impl IInput for EmptyInput {
    fn get_store_id(&self) -> String {
        String::new()
    }
    fn origin(&self) -> String {
        String::new()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HelloInput {
    #[serde(default)]
    pub name: String,
}

impl IInput for HelloInput {
    fn get_store_id(&self) -> String {
        String::new()
    }
    fn origin(&self) -> String {
        String::new()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PingInput {}

impl IInput for PingInput {
    fn get_store_id(&self) -> String {
        String::new()
    }
    fn origin(&self) -> String {
        String::new()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
