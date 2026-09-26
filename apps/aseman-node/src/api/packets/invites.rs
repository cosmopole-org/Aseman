//! Request and response payloads for the `invites` action namespace.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AcceptInput {
    #[serde(rename = "storeId", default)]
    pub store_id: String,
}
