//! Request and response payloads for the `auth` action namespace.

use serde::{Deserialize, Serialize};

use crate::models::input::IInput;

// ---- Inputs ----------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetServerKeyInput {}

impl IInput for GetServerKeyInput {
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
pub struct GetServersMapInput {}

impl IInput for GetServersMapInput {
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

// ---- Outputs ---------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetServerKeyOutput {
    #[serde(rename = "publicKey", default)]
    pub public_key: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetServersMapOutput {
    #[serde(default)]
    pub servers: Vec<String>,
}
