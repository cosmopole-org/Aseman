//! Bridges a container's host-call request onto the node's unified VM
//! host-function surface.
//!
//! A container does *everything* — persisting through DB/storage ops, reaching
//! the outside world through the node's HTTP host function, signalling other
//! creatures, spawning sibling VMs — by sending a `REQUEST` whose payload is:
//!
//! ```json
//! { "op": "<host function name>", "input": { ... } }
//! ```
//!
//! This module injects the connection's authenticated identity into the request
//! (so a container can never spoof another creature's namespace) and forwards
//! it to [`handle_unified_host_call`], the exact same entry point the in-process
//! wasm/elpian runtimes use. The result is returned verbatim as the `RESPONSE`
//! payload.

use crate::drivers::vmm::host::functions::program_target::{
    PROGRAM_TARGET_OPS, TARGET_PROGRAM_ID_KEY,
};
use crate::drivers::vmm::host::functions::vm_ownership::{TARGET_VM_ID_KEY, VM_TARGET_OPS};
use crate::drivers::vmm::host::vm_host_functions::handle_unified_host_call;
use crate::drivers::vmm::network::docker_host::connection::ContainerIdentity;
use crate::drivers::vmm::prelude::*;

/// Execute one host-call request on behalf of `identity` and return the raw
/// JSON response bytes to ship back to the container.
pub(crate) fn dispatch_host_call(identity: &ContainerIdentity, request: &JsonValue) -> Vec<u8> {
    let op = request["op"]
        .as_str()
        .or_else(|| request["key"].as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    if op.is_empty() {
        return error_response("op is required");
    }

    // Start from the caller-supplied input and stamp the connection's verified
    // identity over it. The container cannot override these — they are the
    // server's source of truth for namespacing and auth.
    let mut input = match request.get("input") {
        Some(JsonValue::Object(_)) => request["input"].clone(),
        Some(JsonValue::Null) | None => json!({}),
        // A non-object input (e.g. a bare string) is wrapped so identity can be
        // attached without losing the original value.
        Some(other) => json!({ "value": other.clone() }),
    };
    stamp_request(&op, &mut input, identity);
    match identity_packet(&op, input, identity) {
        Some(packet) => handle_unified_host_call(&packet).into_bytes(),
        None => error_response("the container has no verified identity"),
    }
}

/// The host-call packet, carrying the connection's verified identity at packet level:
/// the only place the host reads a caller's identity from. A container without a
/// verified VM identity makes no host calls.
fn identity_packet(op: &str, input: JsonValue, identity: &ContainerIdentity) -> Option<JsonValue> {
    if identity.vm_id.trim().is_empty() {
        return None;
    }
    let mut packet = json!({ "op": op, "input": input, "vmId": identity.vm_id });
    for (field, value) in [
        ("creatureId", &identity.creature_id),
        ("programId", &identity.program_id),
    ] {
        if !value.is_empty() {
            packet[field] = JsonValue::String(value.clone());
        }
    }
    Some(packet)
}

/// What a host op addresses, when it is not the calling container itself.
///
/// `vmId` and `programId` are overloaded: the node resolves and namespaces the
/// CALLER by them, and the VM and program ops read their TARGET from them.
/// Stamping identity over them pointed every `runVm` a creature made at its own
/// container, and every `deployEntity`/`deleteProgram` at its own program. The
/// target is split out here instead of being overwritten, and identity stamping
/// stays exactly as it was.
fn targets(
    op: &str,
    input: &JsonValue,
    identity: &ContainerIdentity,
) -> Vec<(&'static str, String)> {
    let named_other = |field: &str, own: &str| -> Option<String> {
        let requested = input[field].as_str().unwrap_or("").trim();
        if requested.is_empty() || requested == own {
            return None;
        }
        Some(requested.to_string())
    };
    let mut out = Vec::new();
    if VM_TARGET_OPS.contains(&op) {
        if let Some(target) = named_other("vmId", &identity.vm_id) {
            out.push((TARGET_VM_ID_KEY, target));
        }
    }
    if PROGRAM_TARGET_OPS.contains(&op) {
        if let Some(target) = named_other("programId", &identity.program_id) {
            out.push((TARGET_PROGRAM_ID_KEY, target));
        }
    }
    out
}

/// Stamp identity onto a request, keeping an op's target out of its way.
fn stamp_request(op: &str, input: &mut JsonValue, identity: &ContainerIdentity) {
    // Read BEFORE stamping: stamping overwrites these with the caller's own.
    let targets = targets(op, input, identity);
    stamp_identity(input, identity);
    if let Some(obj) = input.as_object_mut() {
        for (key, target) in targets {
            obj.insert(key.to_string(), JsonValue::String(target));
        }
    }
}

/// Overwrite identity-bearing fields on `input` with the connection's verified
/// identity. Empty identity fields are left untouched so a fully-specified
/// request still works in degraded/standalone setups.
fn stamp_identity(input: &mut JsonValue, identity: &ContainerIdentity) {
    let Some(obj) = input.as_object_mut() else {
        return;
    };
    let set = |obj: &mut serde_json::Map<String, JsonValue>, k: &str, v: &str| {
        if !v.is_empty() {
            obj.insert(k.to_string(), JsonValue::String(v.to_string()));
        }
    };
    set(obj, "vmId", &identity.vm_id);
    set(obj, "creatureId", &identity.creature_id);
    set(obj, "programId", &identity.program_id);
    // The host layer treats `machineId` as the owning program for docker
    // creatures; default it to the program id when the caller omits it.
    if obj
        .get("machineId")
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .is_empty()
    {
        let machine = if !identity.machine_id.is_empty() {
            &identity.machine_id
        } else {
            &identity.program_id
        };
        set(obj, "machineId", machine);
    }
}

fn error_response(msg: &str) -> Vec<u8> {
    json!({ "ok": false, "error": msg })
        .to_string()
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ContainerIdentity {
        ContainerIdentity {
            vm_id: "container-vm".to_string(),
            machine_id: "10@global".to_string(),
            creature_id: "8@global".to_string(),
            program_id: "10@global".to_string(),
            entity_id: "main".to_string(),
        }
    }

    fn stamped(op: &str, mut input: JsonValue) -> JsonValue {
        stamp_request(op, &mut input, &identity());
        input
    }

    #[test]
    fn a_vm_op_keeps_its_target_apart_from_the_callers_identity() {
        let input = stamped("runVm", json!({"vmId": "space-vm", "machineId": "space-1"}));
        assert_eq!(input["vmId"], "container-vm");
        assert_eq!(input[TARGET_VM_ID_KEY], "space-vm");
        assert_eq!(input["machineId"], "space-1");
    }

    #[test]
    fn a_vm_op_naming_the_caller_itself_needs_no_target() {
        let input = stamped("execVm", json!({"vmId": "container-vm"}));
        assert_eq!(input["vmId"], "container-vm");
        assert!(input.get(TARGET_VM_ID_KEY).is_none());
    }

    #[test]
    fn a_non_vm_op_cannot_carry_a_target() {
        let input = stamped("getJson", json!({"vmId": "someone-else"}));
        assert_eq!(input["vmId"], "container-vm");
        assert!(input.get(TARGET_VM_ID_KEY).is_none());
    }

    #[test]
    fn a_program_op_keeps_its_target_apart_from_the_callers_identity() {
        let input = stamped(
            "deployEntity",
            json!({"programId": "proxy-program", "entityId": "main"}),
        );
        assert_eq!(input["programId"], "10@global");
        assert_eq!(input[TARGET_PROGRAM_ID_KEY], "proxy-program");
        let input = stamped("deleteProgram", json!({"programId": "proxy-program"}));
        assert_eq!(input[TARGET_PROGRAM_ID_KEY], "proxy-program");
    }

    #[test]
    fn identity_travels_at_packet_level_and_unidentified_containers_are_refused() {
        let packet = identity_packet("secretGet", json!({"vmId": "victim"}), &identity()).unwrap();
        assert_eq!(packet["vmId"], "container-vm");
        assert_eq!(packet["creatureId"], "8@global");
        assert_eq!(packet["programId"], "10@global");
        let anonymous = ContainerIdentity {
            vm_id: String::new(),
            ..identity()
        };
        assert!(identity_packet("secretGet", json!({"vmId": "victim"}), &anonymous).is_none());
    }

    #[test]
    fn a_program_op_naming_the_caller_itself_needs_no_target() {
        let input = stamped("deployEntity", json!({"programId": "10@global"}));
        assert!(input.get(TARGET_PROGRAM_ID_KEY).is_none());
    }
}
