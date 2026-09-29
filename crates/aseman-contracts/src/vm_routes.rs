//! Characterized Caspar VM HTTP-route compatibility values.
//!
//! New gateway contracts must not adopt caller-controlled link keys as authority. This
//! module exists solely so the deprecated adapter has one tested compatibility owner.

use serde_json::{Value as JsonValue, json};

pub const ROUTE_LINK_NS: &str = "vmHttpRoute";
pub const ROUTE_REV_LINK_NS: &str = "vmHttpRouteFor";
pub const ROUTE_ALIAS_LINK_NS: &str = "vmHttpRouteUser";
pub const MAX_ROUTE_SEGMENTS: usize = 8;

#[must_use]
pub fn normalize_path(path: &str) -> String {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

#[must_use]
pub fn route_link_key(creature_id: &str, path: &str) -> String {
    format!("{ROUTE_LINK_NS}::{creature_id}::{path}")
}

#[must_use]
pub fn route_rev_link_key(program_id: &str, entity_id: &str) -> String {
    format!("{ROUTE_REV_LINK_NS}::{program_id}::{entity_id}")
}

#[must_use]
pub fn route_alias_link_key(local_part: &str) -> String {
    format!("{ROUTE_ALIAS_LINK_NS}::{local_part}")
}

#[must_use]
pub fn username_local_part(username: &str) -> &str {
    username.split('@').next().unwrap_or(username)
}

#[must_use]
pub fn encode_target(program_id: &str, entity_id: &str, vm_id: &str, runtime: &str) -> String {
    json!({ "programId": program_id, "entityId": entity_id, "vmId": vm_id, "runtime": runtime })
        .to_string()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedRoute {
    pub program_id: String,
    pub entity_id: String,
    pub vm_id: String,
    pub runtime: String,
    pub rest_path: String,
}

#[must_use]
pub fn decode_target(stored: &str, rest_segments: &[&str]) -> Option<ResolvedRoute> {
    let value: JsonValue = serde_json::from_str(stored).ok()?;
    let program_id = value["programId"].as_str().unwrap_or("").to_owned();
    let entity_id = value["entityId"].as_str().unwrap_or("").to_owned();
    if program_id.is_empty() || entity_id.is_empty() {
        return None;
    }
    Some(ResolvedRoute {
        program_id,
        entity_id,
        vm_id: value["vmId"].as_str().unwrap_or("").to_owned(),
        runtime: value["runtime"].as_str().unwrap_or("").to_owned(),
        rest_path: format!("/{}", rest_segments.join("/")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_legacy_normalization_and_keys() {
        assert_eq!(normalize_path("/api//v1/"), "api/v1");
        assert_eq!(normalize_path("api/../admin"), "api/../admin");
        assert_eq!(
            route_link_key("creature-1", "api/v1"),
            "vmHttpRoute::creature-1::api/v1"
        );
        assert_eq!(
            route_rev_link_key("program-1", "entity-1"),
            "vmHttpRouteFor::program-1::entity-1"
        );
        assert_eq!(route_alias_link_key("alice"), "vmHttpRouteUser::alice");
        assert_eq!(username_local_part("alice@http://node:4000"), "alice");
    }

    #[test]
    fn target_round_trip_and_legacy_missing_fields() {
        let stored = encode_target("program-1", "entity-1", "vm-1", "docker");
        let route = decode_target(&stored, &["users", "42"]).expect("valid fixture");
        assert_eq!(route.program_id, "program-1");
        assert_eq!(route.rest_path, "/users/42");
        let legacy =
            decode_target(r#"{"programId":"p","entityId":"e"}"#, &[]).expect("legacy fixture");
        assert_eq!(legacy.vm_id, "");
        assert_eq!(legacy.rest_path, "/");
    }

    #[test]
    fn invalid_or_incomplete_target_is_rejected() {
        for value in [
            "not-json",
            r#"{"entityId":"e"}"#,
            r#"{"programId":"p"}"#,
            r#"{"programId":"","entityId":"e"}"#,
        ] {
            assert!(decode_target(value, &[]).is_none());
        }
    }
}
