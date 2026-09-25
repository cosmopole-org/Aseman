use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Error {
    #[serde(rename = "message")]
    pub message: String,
}

pub fn build_error_json(message: &str) -> Error {
    Error {
        message: message.to_string(),
    }
}
