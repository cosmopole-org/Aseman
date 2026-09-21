//! Program ops whose `programId` names the program operated ON, not the caller.
//!
//! The companion of `vm_ownership`'s `targetVmId`. `programId` is overloaded
//! the same way `vmId` is: the docker gateway stamps the CALLER's program id
//! into it, and the program ops read their TARGET from it. So a docker creature
//! that deployed a proxy entity onto a program it had just created deployed it
//! onto ITSELF, a `deleteProgram` of that proxy deleted the creature's own
//! program, and `createProgram` always tried to create the caller's own id and
//! failed with `program already exists`.
//!
//! The gateway moves a caller-named program into `targetProgramId`, and
//! [`apply_program_target`] moves it back once the caller is resolved and
//! authorized against it.

use serde_json::{json, Map, Value};

use crate::drivers::vmm::globals::with_global_app;
use crate::drivers::vmm::host::functions::vm_ownership::program_owner_user;
use crate::models::transaction::ITrx;
use crate::shell::api::model::Creature;

/// Host ops whose `programId` names the program being operated ON.
pub(crate) const PROGRAM_TARGET_OPS: &[&str] = &[
    "deployEntity",
    "deploy entity",
    "deleteProgram",
    "deleteOwnedProgram",
    "updateProgram",
    "getProgram",
];

/// Where a program op's target travels while `programId` carries the caller.
pub(crate) const TARGET_PROGRAM_ID_KEY: &str = "targetProgramId";

/// Ops that only read, and are not narrowed by ownership.
const READ_ONLY_PROGRAM_OPS: &[&str] = &["getProgram"];

/// The user who owns a machine creature, or empty when it has no recorded owner.
pub(crate) fn machine_owner_user(machine_id: &str) -> String {
    let machine_id = machine_id.trim().to_string();
    if machine_id.is_empty() {
        return String::new();
    }
    let slot = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let slot_c = slot.clone();
    with_global_app(|app| {
        app.modify_state(
            true,
            Box::new(move |trx: &dyn ITrx| {
                let machine = (crate::shell::api::model::creature_ports::LegacyCreatures { trx })
                    .creature_or_empty(&machine_id.clone());
                *slot_c.lock().unwrap() = machine.owner_id;
                Ok(())
            }),
        );
    });
    let owner = slot.lock().unwrap().clone();
    owner.trim().to_string()
}

/// Whether `caller` may change `target`: itself, or a program of the same owner.
fn same_owner(caller: &str, target: &str) -> bool {
    if caller == target {
        return true;
    }
    let owner = program_owner_user(target);
    !owner.is_empty() && owner == program_owner_user(caller)
}

/// `createProgram`'s input with the caller's stamped identity taken back out.
///
/// `programId` equal to the caller is the stamp, never a request to create a
/// program that already exists. A `machineId` that is empty or equal to the
/// caller is the host layer's default, so an explicit `appId` wins over it.
fn normalize_create_input(caller: &str, obj: &mut Map<String, Value>) {
    let is_caller = |v: Option<&Value>| v.and_then(Value::as_str).map(str::trim) == Some(caller);
    if !caller.is_empty() && is_caller(obj.get("programId")) {
        obj.remove("programId");
    }
    let app_id = obj
        .get("appId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let machine = obj
        .get("machineId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if let Some(app_id) = app_id {
        if machine.is_empty() || machine == caller {
            obj.insert("machineId".to_string(), Value::String(app_id));
        }
    }
}

fn denied(what: &str) -> String {
    json!({"ok": false, "error": format!("you are not the owner of this {}", what)}).to_string()
}

/// Point a program op at the program its caller named, once the caller is known.
///
/// Returns the error response to send when the caller may not act on it.
pub(crate) fn apply_program_target(
    op: &str,
    caller_program_id: &str,
    input: &mut Value,
) -> Result<(), String> {
    let caller = caller_program_id.trim();
    let Some(obj) = input.as_object_mut() else {
        return Ok(());
    };
    // Removed for every op, so a stray key never reaches a handler.
    let target = obj.remove(TARGET_PROGRAM_ID_KEY);

    if op == "createProgram" {
        normalize_create_input(caller, obj);
        let machine = obj
            .get("machineId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if !machine.is_empty() && machine != caller {
            let owner = machine_owner_user(&machine);
            if !owner.is_empty() && owner != program_owner_user(caller) {
                return Err(denied("machine"));
            }
        }
        return Ok(());
    }

    let target = target
        .as_ref()
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if target.is_empty() || !PROGRAM_TARGET_OPS.contains(&op) {
        return Ok(());
    }
    if !READ_ONLY_PROGRAM_OPS.contains(&op) && !same_owner(caller, &target) {
        return Err(denied("program"));
    }
    obj.insert("programId".to_string(), Value::String(target));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(caller: &str, input: Value) -> Value {
        let mut input = input;
        normalize_create_input(caller, input.as_object_mut().unwrap());
        input
    }

    #[test]
    fn the_callers_stamped_program_id_is_not_the_program_to_create() {
        let input = normalized(
            "10@global",
            json!({"programId": "10@global", "appId": "3@global"}),
        );
        assert!(input.get("programId").is_none());
    }

    #[test]
    fn an_explicit_new_program_id_is_kept() {
        let input = normalized("10@global", json!({"programId": "99@global"}));
        assert_eq!(input["programId"], "99@global");
    }

    #[test]
    fn app_id_wins_over_a_defaulted_machine_id() {
        let input = normalized(
            "10@global",
            json!({"machineId": "10@global", "appId": "3@global"}),
        );
        assert_eq!(input["machineId"], "3@global");
        let input = normalized("10@global", json!({"appId": "3@global"}));
        assert_eq!(input["machineId"], "3@global");
    }

    #[test]
    fn an_explicit_machine_id_wins_over_app_id() {
        let input = normalized(
            "10@global",
            json!({"machineId": "4@global", "appId": "3@global"}),
        );
        assert_eq!(input["machineId"], "4@global");
    }
}
