//! Runtime models for the docker VM plugin.

use serde_json::Value as JsonValue;

/// Identity fields extracted from a docker VM packet.
pub struct DockerIdentity {
    pub entity_id: String,
    pub container_name: String,
    /// Part of the packet schema; not consulted by the controller today.
    #[allow(dead_code)]
    pub standalone: bool,
    pub vm_id: String,
}

impl DockerIdentity {
    pub fn from_packet(packet: &JsonValue) -> Self {
        let standalone = packet["standalone"].as_bool().unwrap_or(false)
            || packet["isStandalone"].as_bool().unwrap_or(false);
        let entity_id = packet["entityId"]
            .as_str()
            .or_else(|| packet["imageName"].as_str())
            .unwrap_or("main")
            .to_string();
        let container_name = packet["containerName"]
            .as_str()
            .unwrap_or("main")
            .to_string();
        let vm_id = packet["vmId"].as_str().unwrap_or("").to_string();
        DockerIdentity {
            entity_id,
            container_name,
            standalone,
            vm_id,
        }
    }
}
