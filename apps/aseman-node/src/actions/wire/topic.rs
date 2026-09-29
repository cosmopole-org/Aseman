//! Wire shapes of the bridge topic operations (`/gateway/*`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewaySubscribeInput {
    /// The bearer token the owning creature minted for this bridge.
    #[serde(default)]
    pub token: String,
    /// Topics to bind. Empty means "everything the grant covers"; a non-empty
    /// list can only narrow it.
    #[serde(default)]
    pub topics: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewayUnsubscribeInput {
    #[serde(default)]
    pub token: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewaySignalInput {
    #[serde(default)]
    pub token: String,
    /// Which of the grant's topics this call belongs to. Optional; when set it
    /// must be one the grant covers.
    #[serde(default)]
    pub topic: String,
    /// The creature action to invoke, e.g. `crew/message`.
    #[serde(default)]
    pub action: String,
    #[serde(rename = "correlationId", default)]
    pub correlation_id: String,
    #[serde(default)]
    pub payload: Value,
}
