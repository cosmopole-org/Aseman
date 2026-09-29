//! Which creature owns a VM instance, and therefore who may delete it.
//!
//! `runVm` is deliberately un-ACL'd — a creature launches VMs freely — but a
//! *delete* is destructive and final, so it needs an owner to check against.
//! The owner is recorded at launch, by the same host op that created the VM,
//! and read back here. The link is written from the node (never from a
//! packet field a guest supplies), so a creature cannot claim a VM it did not
//! start.
//!
//! Two shapes of ownership exist on a `core.vm_instance`, and a delete accepts
//! either:
//!
//!   * `owner_program` — written by the `runVm` host op with the node-resolved
//!     calling program. This is the creature-owns-its-VM case the delete host
//!     op enforces.
//!   * `program_ref` — written by `/programs/runEntity` for a deployed program
//!     entity. A VM launched that way is owned by its program, so that program
//!     (and the shell action's own owner check) may delete it.

use crate::adapters::vmm::globals::with_global_app;
use crate::api::model::vm_runtime;
use crate::core::trx::Trx;

/// Record `program_id` as the owner of `vm_id`. No-op for an empty id, so a
/// runtime that never returned a vm id cannot write a wildcard entry.
pub(crate) fn record_vm_owner(vm_id: &str, program_id: &str) {
    let vm_id = vm_id.trim();
    let program_id = program_id.trim();
    if vm_id.is_empty() || program_id.is_empty() {
        return;
    }
    let (vm_id, program_id) = (vm_id.to_owned(), program_id.to_owned());
    with_global_app(|app| {
        app.modify_state(
            false,
            Box::new(move |trx: &Trx| vm_runtime::record_owner(trx, &vm_id, &program_id)),
        );
    });
}

/// The program that launched `vm_id`, or an empty string when none was
/// recorded (a VM launched before this registry existed, or by the program
/// API rather than the host op).
pub(crate) fn vm_owner_program(vm_id: &str) -> String {
    let vm_id = vm_id.trim();
    if vm_id.is_empty() {
        return String::new();
    }
    let vm_id = vm_id.to_owned();
    let slot = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let slot_c = slot.clone();
    with_global_app(|app| {
        app.modify_state(
            true,
            Box::new(move |trx: &Trx| {
                *slot_c.lock().unwrap() = vm_runtime::instance(trx, &vm_id)?
                    .and_then(|instance| instance.owner_program)
                    .unwrap_or_default();
                Ok(())
            }),
        );
    });
    let owner = slot.lock().unwrap().clone();
    owner.trim().to_string()
}

/// The user who owns a program, resolved through the program's owning machine
/// creature — the same resolution `/programs/runEntity` authorizes against.
///
/// This is the granularity a delete is checked at, and it has to be. A
/// deployment is not one program: every creature action is its own machine +
/// program (the Decillion server deploys ~90 of them), so the creature that
/// creates a resource and the one that tears it down are different programs
/// belonging to the same owner. Checking the *program* id would mean a space's
/// delete action could never remove the sandbox its create action made, while
/// checking the owner still refuses another tenant's creature entirely.
pub(crate) fn program_owner_user(program_id: &str) -> String {
    let program_id = program_id.trim();
    if program_id.is_empty() {
        return String::new();
    }
    let program_id = program_id.to_string();
    let slot = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let slot_c = slot.clone();
    with_global_app(|app| {
        app.modify_state(
            true,
            Box::new(move |trx: &Trx| {
                if aseman_ports::ProgramDirectory::program(
                    &crate::api::model::program_ports::ProgramPorts { trx },
                    &program_id,
                )
                .map_err(|error| anyhow::anyhow!("{error}"))?
                .is_none()
                {
                    return Ok(());
                }
                let program = (crate::api::model::program_ports::ProgramPorts { trx })
                    .program_or_empty(&program_id.clone());
                let machine =
                    crate::api::actions::program::resolve_program_owner_machine(trx, &program);
                *slot_c.lock().unwrap() = machine.owner_id;
                Ok(())
            }),
        );
    });
    let owner = slot.lock().unwrap().clone();
    owner.trim().to_string()
}

/// Whether `program_id` launched `vm_id` as a program *entity*
/// (`/programs/runEntity`), which records the instance's program rather than an
/// owner program.
pub(crate) fn owns_vm_instance(program_id: &str, vm_id: &str) -> bool {
    let program_id = program_id.trim();
    let vm_id = vm_id.trim();
    if program_id.is_empty() || vm_id.is_empty() {
        return false;
    }
    let (program_id, vm_id) = (program_id.to_owned(), vm_id.to_owned());
    let found = std::sync::Arc::new(std::sync::Mutex::new(false));
    let found_c = found.clone();
    with_global_app(|app| {
        app.modify_state(
            true,
            Box::new(move |trx: &Trx| {
                *found_c.lock().unwrap() = vm_runtime::instance(trx, &vm_id)?
                    .is_some_and(|instance| instance.program_ref.as_deref() == Some(&*program_id));
                Ok(())
            }),
        );
    });

    *found.lock().unwrap()
}

pub(crate) use aseman_contracts::guest_api::{TARGET_VM_ID_KEY, VM_TARGET_OPS};

/// Whether `caller_program_id` may act on `vm_id`: the program that launched
/// it, a sibling program of the same owner, or the program `/programs/runEntity`
/// launched it for. The rule `deleteVm` and `vmEndpoints` enforce.
pub(crate) fn caller_owns_vm(caller_program_id: &str, vm_id: &str) -> bool {
    let caller = caller_program_id.trim();
    let vm_id = vm_id.trim();
    if caller.is_empty() || vm_id.is_empty() {
        return false;
    }
    let owner = vm_owner_program(vm_id);
    if !owner.is_empty() {
        return owner == caller || {
            let owner_user = program_owner_user(&owner);
            !owner_user.is_empty() && owner_user == program_owner_user(caller)
        };
    }
    owns_vm_instance(caller, vm_id)
}

/// Point a VM op at the VM its caller named, once the caller is known.
///
/// Returns the error response to send when the caller may not address that VM.
/// Addressing another creature's VM is refused; launching a VM nobody has
/// claimed is allowed, and `runVm` records the caller as its owner.
pub(crate) fn apply_vm_target(
    op: &str,
    caller_program_id: &str,
    input: &mut serde_json::Value,
) -> Result<(), String> {
    let Some(obj) = input.as_object_mut() else {
        return Ok(());
    };
    // Removed for every op, so a stray key never reaches a runtime.
    let Some(target) = obj.remove(TARGET_VM_ID_KEY) else {
        return Ok(());
    };
    let target = target.as_str().unwrap_or("").trim().to_string();
    if target.is_empty() || !VM_TARGET_OPS.contains(&op) {
        return Ok(());
    }
    let allowed = caller_owns_vm(caller_program_id, &target)
        || (op == "runVm" && vm_owner_program(&target).is_empty());
    if !allowed {
        return Err(serde_json::json!({
            "ok": false,
            "error": "you are not the owner of this vm",
        })
        .to_string());
    }
    obj.insert("vmId".to_string(), serde_json::Value::String(target));
    Ok(())
}

/// Forget a deleted VM's instance record. Called after the runtime has actually
/// destroyed the instance, so a failed delete does not orphan a still-running VM
/// by forgetting who owns it.
pub(crate) fn clear_vm_records(vm_id: &str) {
    let vm_id = vm_id.trim().to_string();
    if vm_id.is_empty() {
        return;
    }
    with_global_app(|app| {
        app.modify_state(
            false,
            Box::new(move |trx: &Trx| vm_runtime::forget(trx, &vm_id)),
        );
    });
}
