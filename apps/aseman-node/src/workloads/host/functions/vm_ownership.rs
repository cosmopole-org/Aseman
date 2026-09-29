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

use std::sync::Arc;

use crate::node::Node;
use anyhow::anyhow;
use aseman_ports::ProgramDirectory;

use crate::state::program_ports::{ProgramPorts, program_view};
use crate::state::vm_runtime;

/// Record `program_id` as the owner of `vm_id`. No-op for an empty id, so a
/// runtime that never returned a vm id cannot write a wildcard entry.
pub(crate) fn record_vm_owner(node: &Arc<Node>, vm_id: &str, program_id: &str) {
    let (vm_id, program_id) = (vm_id.trim(), program_id.trim());
    if vm_id.is_empty() || program_id.is_empty() {
        return;
    }
    if let Err(error) = node.in_action(|trx| vm_runtime::record_owner(trx, vm_id, program_id)) {
        eprintln!("cannot record the owner of vm {vm_id}: {error}");
    }
}

/// The program that launched `vm_id`, or an empty string when none was recorded
/// (a VM launched by the program API rather than the host op).
pub(crate) fn vm_owner_program(node: &Arc<Node>, vm_id: &str) -> String {
    let vm_id = vm_id.trim();
    if vm_id.is_empty() {
        return String::new();
    }
    node.read(|trx| {
        Ok(vm_runtime::instance(trx, vm_id)?
            .and_then(|instance| instance.owner_program)
            .unwrap_or_default())
    })
    .unwrap_or_default()
    .trim()
    .to_owned()
}

/// The user who owns a program, resolved through the program's machine — the
/// same resolution `/programs/runEntity` authorizes against.
///
/// This is the granularity a delete is checked at, and it has to be. A deployment
/// is not one program: every creature action is its own machine and program, so
/// the program that creates a resource and the one that tears it down differ but
/// belong to the same owner; checking the owner still refuses another tenant.
pub(crate) fn program_owner_user(node: &Arc<Node>, program_id: &str) -> String {
    let program_id = program_id.trim();
    if program_id.is_empty() {
        return String::new();
    }
    node.read(|trx| {
        let Some(record) = ProgramPorts { trx }
            .program(program_id)
            .map_err(|error| anyhow!("{error}"))?
        else {
            return Ok(String::new());
        };
        Ok(crate::actions::program::owner_machine(trx, &program_view(record)).owner_id)
    })
    .unwrap_or_default()
    .trim()
    .to_owned()
}

/// Whether `program_id` launched `vm_id` as a program *entity*
/// (`/programs/runEntity`), which records the instance's program rather than an
/// owner program.
pub(crate) fn owns_vm_instance(node: &Arc<Node>, program_id: &str, vm_id: &str) -> bool {
    let (program_id, vm_id) = (program_id.trim(), vm_id.trim());
    if program_id.is_empty() || vm_id.is_empty() {
        return false;
    }
    node.read(|trx| {
        Ok(vm_runtime::instance(trx, vm_id)?
            .is_some_and(|instance| instance.program_ref.as_deref() == Some(program_id)))
    })
    .unwrap_or(false)
}

pub(crate) use aseman_contracts::guest_api::{TARGET_VM_ID_KEY, VM_TARGET_OPS};

/// Whether `caller_program_id` may act on `vm_id`: the program that launched
/// it, a sibling program of the same owner, or the program `/programs/runEntity`
/// launched it for. The rule `deleteVm` and `vmEndpoints` enforce.
pub(crate) fn caller_owns_vm(node: &Arc<Node>, caller_program_id: &str, vm_id: &str) -> bool {
    let caller = caller_program_id.trim();
    let vm_id = vm_id.trim();
    if caller.is_empty() || vm_id.is_empty() {
        return false;
    }
    let owner = vm_owner_program(node, vm_id);
    if !owner.is_empty() {
        return owner == caller || {
            let owner_user = program_owner_user(node, &owner);
            !owner_user.is_empty() && owner_user == program_owner_user(node, caller)
        };
    }
    owns_vm_instance(node, caller, vm_id)
}

/// Point a VM op at the VM its caller named, once the caller is known.
///
/// Returns the error response to send when the caller may not address that VM.
/// Addressing another creature's VM is refused; launching a VM nobody has
/// claimed is allowed, and `runVm` records the caller as its owner.
pub(crate) fn apply_vm_target(
    node: &Arc<Node>,
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
    let allowed = caller_owns_vm(node, caller_program_id, &target)
        || (op == "runVm" && vm_owner_program(node, &target).is_empty());
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

/// Forget a deleted VM's instance record. Called after the runtime destroyed the
/// instance, so a failed delete does not orphan a running VM by forgetting its
/// owner.
pub(crate) fn clear_vm_records(node: &Arc<Node>, vm_id: &str) {
    let vm_id = vm_id.trim();
    if vm_id.is_empty() {
        return;
    }
    if let Err(error) = node.in_action(|trx| vm_runtime::forget(trx, vm_id)) {
        eprintln!("cannot forget vm {vm_id}: {error}");
    }
}
