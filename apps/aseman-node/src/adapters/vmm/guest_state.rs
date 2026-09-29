//! Guest document state (LD-24, ADR 0028): the `putJson`/`getJson`/`getByPrefix`/
//! `delKey`/`getLink` host calls, confined to the calling creature.
//!
//! The caller's creature comes from the node: the packet a runtime or the docker
//! gateway stamped, or the VM context registered for a runtime transaction. The guest's
//! key only ever extends the creature's own prefix:
//! - documents live in the creature's `json` guest namespace;
//! - `getLink` reads the creature's own `dbOp` pairs.
//!
//! Before this, the calls addressed arbitrary node keys (finance, sessions, secrets,
//! custodial keys). Operations without a trusted creature are refused.

use serde_json::Value;

/// The guest state operations.
#[expect(
    dead_code,
    reason = "RL-006/RL-013: legacy VM host-call surface kept for the ADR-0004 window"
)]
pub(crate) const GUEST_STATE_OPS: [&str; 5] =
    ["putJson", "getJson", "getByPrefix", "delKey", "getLink"];

/// Run one guest state operation for `creature`: its own guest database, or the
/// node's storage without a guest data plane (ADR 0036).
///
/// # Errors
///
/// A refusal without a trusted creature, a missing field, or an unknown operation.
pub(crate) fn run(creature: &str, op: &str, input: &Value) -> Result<Value, String> {
    if creature.trim().is_empty() || creature.contains("::") {
        return Err("guest state needs an identified creature".to_owned());
    }
    crate::api::model::guest_data::route_state(creature, op, input)
        .unwrap_or_else(|| Err("guest data is not available yet".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_runs_without_a_trusted_creature() {
        for creature in ["", "  ", "a::b"] {
            assert_eq!(
                run(creature, "getJson", &serde_json::json!({"key": "counter"})),
                Err("guest state needs an identified creature".to_owned())
            );
        }
    }
}
