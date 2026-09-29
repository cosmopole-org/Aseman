//! Translation of `shell/api/actions/program/program.go`.
//!
//! Wires the program / app / VM action surface and translates the Go bodies
//! one-to-one. A few notes on the things that didn't survive the port verbatim:
//!
//! * **Per-minute billing background loop.** Go's `Install` spawned a
//!   15-second ticker calling `chargeRunningStandaloneVmsIfNeeded`, which
//!   advances locked-token billing for every running standalone VM. The Rust
//!   port mirrors that ticker using a background thread.
//! * **Chain re-entry for `consumeLock`.** The billing helper now synchronously
//!   calls `Globe.SendBaseRequestOnChain("/creatures/consumeLock", ...)` and
//!   updates VM billing state on success.
//! * The Go module also boots the existing programs (calling `Vmm.Assign` and
//!   replaying any pending `vmAlarm*` links). The Rust `install` mirrors the
//!   one-shot scan; the timed alarm replay is preserved.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::api::model::entity_ports::EntityPorts;
use crate::api::model::vm_runtime;
use crate::api::model::{Creature, Program};
use crate::api::packets::plugin::PlugInput;
use crate::api::packets::program::{
    CreateMachineInput, DeleteProgramInput, DeployInput, DownloadEntityInput, ListAppMachsInput,
    ListInput, MachineBuildsInput, ReadVmLogsInput, RunProgramEntityInput, UpdateProgramInput,
    VmResourcesInput, VmTerminalInput,
};
use crate::api::utils::future::async_once;
use crate::core::actor::Guard;
use crate::core::trx::Trx;
use crate::models::action::ISecureAction;
use crate::models::core::ICore;
use crate::models::state::IState;
use aseman_domain::program::{ArtifactRole, EntityRecord};
use aseman_ports::{BlobStore, EntityDirectory};

use super::util::build_secure_action;

#[expect(
    dead_code,
    reason = "RL-004: characterized legacy action surface (A008) kept until its deletion gate"
)]
const PLUGINS_TEMPLATE_NAME: &str = "/machines/";

fn user_guard() -> Guard {
    Guard {
        is_user: true,
        is_in_store: false,
        allow_applet_sign: true,
    }
}

type Serve<I> = fn(&Arc<dyn ICore>, &Trx, &str, I) -> Result<Value>;

/// A legacy packet action whose body is the shared public-executor body `serve`,
/// run in the action's transaction for the calling creature.
fn served<I>(app: Arc<dyn ICore>, key: &str, serve: Serve<I>) -> Arc<dyn ISecureAction>
where
    I: crate::models::input::IInput
        + serde::de::DeserializeOwned
        + serde::Serialize
        + Default
        + Clone
        + Send
        + Sync
        + 'static,
{
    let app_for_handler = app.clone();
    build_secure_action::<I, _>(app, key, user_guard(), move |state, input: I| {
        serve(
            &app_for_handler,
            &state.trx(),
            &state.info().user_id(),
            input,
        )
    })
}

fn normalize_entity_type(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Resolve the machine that owns a program.
///
/// `Program::machine_id` is canonical for current state. Older/restored state can
/// contain a damaged program row while still retaining the immutable
/// `machinePrograms::<machine>::<program>` link written by `/programs/create` in
/// the same transaction. In that case, use the link as a compatibility fallback.
/// We deliberately fail closed when no linked owner exists or more than one
/// linked machine is found.
pub(crate) fn resolve_program_owner_machine(trx: &Trx, program: &Program) -> Creature {
    let canonical = (crate::api::model::creature_ports::CreaturePorts { trx })
        .creature_or_empty(&program.machine_id.clone());
    if !canonical.owner_id.is_empty() {
        return canonical;
    }

    // The derived `machinePrograms` link always names `program.machine_id` (the
    // legacy adapter maintains it, and the A308 export verifies it), so a reverse
    // scan cannot find another owner.
    Creature::default()
}

/// Read a program entity record through the entity port, or `None`.
#[expect(
    dead_code,
    reason = "RL-004: characterized legacy action surface (A008) kept until its deletion gate"
)]
pub(crate) fn read_program_entity(
    trx: &Trx,
    program_id: &str,
    entity_id: &str,
) -> Result<Option<aseman_domain::program::EntityRecord>> {
    let entities = EntityPorts { trx };
    aseman_ports::EntityDirectory::entity(&entities, program_id, entity_id)
        .map_err(|error| anyhow!("{error}"))
}

/// Normalized VM launch resources (defaults for non-positive fields).
#[expect(
    dead_code,
    reason = "RL-004: characterized legacy action surface (A008) kept until its deletion gate"
)]
pub(crate) fn normalized_vm_resources(input: &VmResourcesInput) -> VmResources {
    normalize_vm_resources(input)
}

/// Build the legacy per-minute billing record for a standalone VM launch.
#[expect(
    dead_code,
    reason = "RL-004: characterized legacy action surface (A008) kept until its deletion gate"
)]
pub(crate) fn build_vm_billing(
    app: &Arc<dyn ICore>,
    trx: &Trx,
    payer_id: &str,
    lock_id: &str,
    payment_signatures: &[String],
    resources: &VmResources,
) -> Result<Map<String, Value>> {
    validate_and_build_vm_billing(app, trx, payer_id, lock_id, payment_signatures, resources)
}

fn as_i64(raw: &Value) -> Option<i64> {
    match raw {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct VmResources {
    #[serde(rename = "maxExecTimeSeconds")]
    max_exec_time_seconds: i64,
    #[serde(rename = "ramMb")]
    ram_mb: i64,
    #[serde(rename = "diskGb")]
    disk_gb: i64,
    #[serde(rename = "cpuCores")]
    cpu_cores: i64,
}

fn normalize_vm_resources(input: &VmResourcesInput) -> VmResources {
    let mut res = VmResources {
        max_exec_time_seconds: input.max_exec_time_seconds,
        ram_mb: input.ram_mb,
        disk_gb: input.disk_gb,
        cpu_cores: input.cpu_cores,
    };
    if res.max_exec_time_seconds <= 0 {
        res.max_exec_time_seconds = 60;
    }
    if res.ram_mb <= 0 {
        res.ram_mb = 64;
    }
    if res.disk_gb <= 0 {
        res.disk_gb = 1;
    }
    if res.cpu_cores <= 0 {
        res.cpu_cores = 1;
    }
    res
}

fn vm_per_minute_cost(app: &Arc<dyn ICore>, resources: &VmResources) -> i64 {
    let cost = (resources.ram_mb * app.vm_ram_cost_per_mb_per_minute())
        + (resources.cpu_cores * app.vm_cpu_core_cost_per_minute())
        + (resources.disk_gb * app.vm_disk_cost_per_gb_per_minute());
    if cost <= 0 { 1 } else { cost }
}

/// Whether the node is free-tier (every VM cost rate is zero).
pub(crate) fn vm_is_free(app: &Arc<dyn ICore>) -> bool {
    app.vm_ram_cost_per_mb_per_minute() == 0
        && app.vm_cpu_core_cost_per_minute() == 0
        && app.vm_disk_cost_per_gb_per_minute() == 0
}

fn validate_and_build_vm_billing(
    app: &Arc<dyn ICore>,
    trx: &Trx,
    payer_id: &str,
    lock_id: &str,
    payment_signatures: &[String],
    resources: &VmResources,
) -> Result<Map<String, Value>> {
    if lock_id.is_empty() {
        return Err(anyhow!(
            "paymentLockId is required for standalone vm execution"
        ));
    }
    let payment = crate::api::model::token_locks::lock(trx, payer_id, lock_id)?
        .ok_or_else(|| anyhow!("payment lock not found"))?;
    let target = payment.get("userId").and_then(|v| v.as_str()).unwrap_or("");
    if target != app.owner_id() {
        return Err(anyhow!("payment lock target is invalid"));
    }
    let steps_raw = match payment.get("steps") {
        Some(Value::Array(arr)) if !arr.is_empty() => arr.clone(),
        _ => return Err(anyhow!("payment lock does not include steps")),
    };
    if payment_signatures.len() != steps_raw.len() {
        return Err(anyhow!(
            "paymentSignatures count must match lock steps count"
        ));
    }
    let per_minute_cost = vm_per_minute_cost(app, resources);
    let mut step_unlocks = vec![0i64; steps_raw.len()];
    for (i, raw_step) in steps_raw.iter().enumerate() {
        let step = match raw_step {
            Value::Object(o) => o,
            _ => return Err(anyhow!("invalid payment lock step")),
        };
        let step_amount = step.get("amount").and_then(as_i64).unwrap_or(0);
        if step_amount != per_minute_cost {
            return Err(anyhow!(
                "payment lock step amount must match vm per-minute resource cost"
            ));
        }
        let unlock_at = step.get("unlockAt").and_then(as_i64).unwrap_or(0);
        if unlock_at <= 0 {
            return Err(anyhow!("payment lock step unlockAt is invalid"));
        }
        step_unlocks[i] = unlock_at;
        if i > 0 && (step_unlocks[i] - step_unlocks[i - 1] != 60_000) {
            return Err(anyhow!("payment lock steps must be one-minute apart"));
        }
        let sign_payload = format!(
            "{}:{}:{}:{}:{}",
            lock_id,
            i,
            unlock_at,
            step_amount,
            app.owner_id()
        );
        let (success, _, _) = app.tools().security().auth_with_signature(
            payer_id,
            sign_payload.as_bytes(),
            &payment_signatures[i],
        );
        if !success {
            return Err(anyhow!("payment signature verification failed"));
        }
    }
    let mut out: Map<String, Value> = Map::new();
    out.insert("payerUserId".into(), json!(payer_id));
    out.insert("lockId".into(), json!(lock_id));
    out.insert("perMinuteCost".into(), json!(per_minute_cost));
    out.insert("currentStep".into(), json!(0));
    out.insert("stepCount".into(), json!(steps_raw.len()));
    out.insert("lastChargeMinute".into(), json!(-1i64));
    out.insert("signatures".into(), json!(payment_signatures));
    out.insert("resources".into(), serde_json::to_value(resources)?);
    Ok(out)
}

/// The per-minute VM billing sweep (started by `install`): every running instance
/// that carries billing is charged one step through its payment lock, or stopped
/// when its signed steps are spent or a charge fails.
pub(crate) fn charge_running_standalone_vms_if_needed(app: &Arc<dyn ICore>, lock: &Mutex<i64>) {
    let mut guard = match lock.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let now = chrono::Utc::now().timestamp();
    let current_minute = now / 60;
    if *guard == current_minute {
        return;
    }
    let targets: Arc<Mutex<Vec<Map<String, Value>>>> = Arc::new(Mutex::new(Vec::new()));
    let targets_for_closure = targets.clone();
    app.modify_state(
        true,
        Box::new(move |tx: &Trx| {
            let Ok(instances) = vm_runtime::billed_running(tx) else {
                return Ok(());
            };
            let mut acc = targets_for_closure.lock().unwrap();
            for instance in instances {
                let vm_id = instance.key.clone();
                let Some(billing) = instance
                    .billing
                    .and_then(|billing| billing.as_object().cloned())
                else {
                    continue;
                };
                let next_step = match billing.get("currentStep").and_then(as_i64) {
                    Some(n) => n,
                    None => continue,
                };
                let payer_id = billing
                    .get("payerUserId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let lock_id = billing
                    .get("lockId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let per_minute_cost = billing.get("perMinuteCost").and_then(as_i64).unwrap_or(0);
                let last_charge_minute = billing
                    .get("lastChargeMinute")
                    .and_then(as_i64)
                    .unwrap_or(0);
                let machine_id = billing
                    .get("machineId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let entity_id = billing
                    .get("entityId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let signatures_raw: Vec<String> = billing
                    .get("signatures")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|s| s.as_str().unwrap_or("").to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                if payer_id.is_empty()
                    || lock_id.is_empty()
                    || per_minute_cost <= 0
                    || machine_id.is_empty()
                    || entity_id.is_empty()
                {
                    continue;
                }
                if last_charge_minute == current_minute {
                    continue;
                }
                if next_step < 0 {
                    continue;
                }
                if (next_step as usize) >= signatures_raw.len() {
                    if last_charge_minute < current_minute {
                        let mut t: Map<String, Value> = Map::new();
                        t.insert("vmId".into(), json!(vm_id));
                        t.insert("machineId".into(), json!(machine_id));
                        t.insert("entityId".into(), json!(entity_id));
                        t.insert("stopOnly".into(), json!(true));
                        acc.push(t);
                    }
                    continue;
                }
                let mut t: Map<String, Value> = Map::new();
                t.insert("vmId".into(), json!(vm_id));
                t.insert("payerUserId".into(), json!(payer_id));
                t.insert("lockId".into(), json!(lock_id));
                t.insert("step".into(), json!(next_step));
                t.insert("amount".into(), json!(per_minute_cost));
                t.insert(
                    "signature".into(),
                    json!(signatures_raw[next_step as usize]),
                );
                t.insert("machineId".into(), json!(machine_id));
                t.insert("entityId".into(), json!(entity_id));
                acc.push(t);
            }
            Ok(())
        }),
    );
    let targets = std::mem::take(&mut *targets.lock().unwrap());
    for target in targets {
        if target
            .get("stopOnly")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let machine_id = target
                .get("machineId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let entity_id = target
                .get("entityId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let vm_id = target
                .get("vmId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            terminate_standalone_vm(app, &machine_id, &entity_id, &vm_id);
            let vm_id_for_closure = vm_id.clone();
            let machine_id_for_closure = machine_id.clone();
            let entity_id_for_closure = entity_id.clone();
            app.modify_state(
                false,
                Box::new(move |tx: &Trx| {
                    let _ = (&machine_id_for_closure, &entity_id_for_closure);
                    vm_runtime::mark_stopped(tx, &vm_id_for_closure)
                }),
            );
            continue;
        }
        let payload = json!({
            "type": "pay",
            "userId": target.get("payerUserId").and_then(|v| v.as_str()).unwrap_or(""),
            "lockId": target.get("lockId").and_then(|v| v.as_str()).unwrap_or(""),
            "signature": target.get("signature").and_then(|v| v.as_str()).unwrap_or(""),
            "amount": target.get("amount").and_then(as_i64).unwrap_or(0),
            "step": target.get("step").and_then(as_i64).unwrap_or(-1),
        });
        let payload_bytes = serde_json::to_vec(&payload).unwrap_or_default();
        let sig = app.sign_packet_as_owner(&payload_bytes);
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        let owner = app.owner_id();
        app.globe().send_base_request_on_chain(
            "/creatures/consumeLock",
            payload_bytes,
            &sig,
            &owner,
            "",
            Box::new(move |_data, status, err| {
                let ok = err.is_none() && status < 400;
                let _ = tx.send(ok);
            }),
        );
        let consumed = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap_or(false);
        let vm_id = target
            .get("vmId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let machine_id = target
            .get("machineId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let entity_id = target
            .get("entityId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if consumed {
            let vm_for_closure = vm_id.clone();
            app.modify_state(
                false,
                Box::new(move |tx: &Trx| {
                    let mut billing = vm_runtime::instance(tx, &vm_for_closure)?
                        .and_then(|instance| instance.billing)
                        .and_then(|billing| billing.as_object().cloned())
                        .unwrap_or_default();
                    let current_step = billing.get("currentStep").and_then(as_i64).unwrap_or(0);
                    billing.insert("currentStep".into(), json!(current_step + 1));
                    billing.insert("lastChargeMinute".into(), json!(current_minute));
                    vm_runtime::set_billing(tx, &vm_for_closure, billing)
                }),
            );
        } else {
            terminate_standalone_vm(app, &machine_id, &entity_id, &vm_id);
        }
    }
    *guard = current_minute;
}

/// Stop one standalone instance (the billing reaper): a desired-state change made
/// as the program's owning creature, whose ownership the node established.
fn terminate_standalone_vm(app: &Arc<dyn ICore>, machine_id: &str, entity_id: &str, vm_id: &str) {
    let Some(remote) = crate::api::workloads::remote() else {
        eprintln!("cannot stop {machine_id}/{entity_id}/{vm_id}: this node has no VMM");
        return;
    };
    let owner = crate::api::workloads::program_machine(app, machine_id);
    if let Err(error) = remote.set_state_as(
        crate::api::workloads::creature_subject(&owner),
        crate::api::workloads::RemoteWorkloads::workload_id(machine_id, entity_id, vm_id),
        aseman_domain::DesiredWorkloadState::Stopped,
    ) {
        eprintln!("cannot stop {machine_id}/{entity_id}/{vm_id}: {error}");
    }
}

/// A program's entity, read through the entity port.
fn read_entity(trx: &Trx, program_id: &str, entity_id: &str) -> Result<Option<EntityRecord>> {
    EntityPorts { trx }
        .entity(program_id, entity_id)
        .map_err(|error| anyhow!("{error}"))
}

fn create_program(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<CreateMachineInput, _>(
        app,
        "/programs/create",
        user_guard(),
        move |state: Arc<dyn IState>, input: CreateMachineInput| -> Result<Value> {
            let trx = state.trx();
            let creatures = crate::api::model::creature_ports::CreaturePorts { trx: &trx };
            let programs = crate::api::model::program_ports::ProgramPorts { trx: &trx };
            let created = aseman_application::program::CreateProgram {
                creatures: &creatures,
                programs: &programs,
            }
            .execute(
                &state.info().user_id(),
                aseman_application::program::NewProgram {
                    id: app_for_handler
                        .tools()
                        .storage()
                        .gen_id(&crate::models::input::IInput::origin(&input)),
                    machine_id: input.app_id.clone(),
                    runtime: input.runtime.clone(),
                    path: input.path.clone(),
                    comment: input.comment.clone(),
                },
            )
            .map_err(crate::api::model::store_ports::legacy_error)?;
            programs
                .merge_metadata_value(&created.id, &json!({}))
                .map_err(|error| anyhow!("{error}"))?;
            let program = crate::api::model::program_ports::program_view(created);
            Ok(json!({"program": program}))
        },
    )
}

fn delete_program(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<DeleteProgramInput, _>(
        app,
        "/programs/delete",
        user_guard(),
        move |state: Arc<dyn IState>, input: DeleteProgramInput| -> Result<Value> {
            let trx = state.trx();
            let programs = crate::api::model::program_ports::ProgramPorts { trx: &trx };
            // LD-17: the program and its relation are really removed; LD-18: only the
            // owner of the program's machine may delete it.
            aseman_application::program::DeleteProgram {
                creatures: &crate::api::model::creature_ports::CreaturePorts { trx: &trx },
                programs: &programs,
            }
            .execute(&state.info().user_id(), &input.program_id)
            .map_err(crate::api::model::store_ports::legacy_error)?;
            aseman_ports::ProgramMetadata::delete_program_metadata(&programs, &input.program_id)
                .map_err(|error| anyhow!("{error}"))?;
            Ok(json!({}))
        },
    )
}

fn update_program(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<UpdateProgramInput, _>(
        app,
        "/programs/update",
        user_guard(),
        move |state: Arc<dyn IState>, input: UpdateProgramInput| -> Result<Value> {
            let trx = state.trx();
            let programs = crate::api::model::program_ports::ProgramPorts { trx: &trx };
            // LD-18: only the owner of the program's machine may change it.
            let program = aseman_application::program::UpdateProgramPath {
                creatures: &crate::api::model::creature_ports::CreaturePorts { trx: &trx },
                programs: &programs,
            }
            .execute(&state.info().user_id(), &input.program_id, &input.path)
            .map_err(crate::api::model::store_ports::legacy_error)?;
            if !input.metadata.is_empty() {
                let meta_value = Value::Object(
                    input
                        .metadata
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                );
                programs
                    .merge_metadata_value(&program.id, &meta_value)
                    .map_err(|error| anyhow!("{error}"))?;
            }
            Ok(json!({}))
        },
    )
}

fn run_program_entity(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    served::<RunProgramEntityInput>(app, "/programs/runEntity", serve_run_program_entity)
}

fn stop_program_entity(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    served::<RunProgramEntityInput>(app, "/programs/stopEntity", serve_stop_program_entity)
}

/// `/programs/deleteEntity` — permanently destroy one VM instance of a
/// deployed program entity.
///
/// This is the destructive sibling of `/programs/stopEntity`. Stop suspends:
/// the instance can be resumed and its persistent volume survives, which is
/// why a stopped VM still leaves its container, sandbox or disk behind.
/// Delete asks the runtime to destroy the instance and everything it owns,
/// then drops every state link that described it — the VM cannot come back.
///
/// Access control matches `stopEntity` exactly: only the recorded owner of the
/// program may delete its instances. The owner is resolved from the program
/// record (never the deprecated `app_id` pointer), so a creature that merely
/// knows a vm id cannot destroy somebody else's VM.
fn delete_program_entity(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    served::<RunProgramEntityInput>(app, "/programs/deleteEntity", serve_delete_program_entity)
}

fn read_vm_logs(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    served::<ReadVmLogsInput>(app, "/machines/readVmLogs", serve_read_vm_logs)
}

/// List the VM instances recorded for one program entity and ask its runtime
/// plugin for the current process/container state.
fn list_entity_vms(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    served::<RunProgramEntityInput>(app, "/machines/listEntityVms", serve_list_entity_vms)
}

fn open_vm_terminal(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    served::<VmTerminalInput>(app, "/machines/openVmTerminal", serve_open_vm_terminal)
}

fn close_vm_terminal(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    served::<VmTerminalInput>(app, "/machines/closeVmTerminal", serve_close_vm_terminal)
}

fn read_machine_builds(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    served::<MachineBuildsInput>(
        app,
        "/machines/readMachineBuilds",
        serve_read_machine_builds,
    )
}

/// Record (or clear) an entity's custom VM gateway route, reconciling any route
/// a previous deploy of the same entity left behind. `creature_id` is the
/// program's owning creature (whose username the route is reached by),
/// `gateway_path` the normalized prefix (empty ⇒ the entity exposes no custom
/// route) and `gateway_vm_id` an optional specific instance to target.
pub(crate) fn register_gateway_route(
    trx: &Trx,
    creature_id: &str,
    program_id: &str,
    entity_id: &str,
    gateway_path: &str,
    gateway_vm_id: &str,
    runtime: &str,
) -> Result<()> {
    use aseman_ports::GatewayRoutes;
    let routes = crate::api::model::gateway_ports::GatewayPorts { trx };
    let failed = |error: aseman_ports::PortError| anyhow!("{error}");
    // Drop a stale route from a prior deploy whose path changed or was removed.
    if let Some((prev_creature, prev_path)) = routes
        .route_of_entity(program_id, entity_id)
        .map_err(failed)?
    {
        let unchanged =
            !gateway_path.is_empty() && prev_creature == creature_id && prev_path == gateway_path;
        if !unchanged {
            routes
                .delete_route(&prev_creature, &prev_path)
                .map_err(failed)?;
        }
    }
    if gateway_path.is_empty() || creature_id.is_empty() {
        return Ok(());
    }
    routes
        .put_route(&aseman_domain::gateway::GatewayRoute {
            creature_id: creature_id.to_owned(),
            path: gateway_path.to_owned(),
            program_id: program_id.to_owned(),
            entity_id: entity_id.to_owned(),
            runtime: runtime.to_owned(),
            pinned_vm_id: gateway_vm_id.to_owned(),
        })
        .map_err(failed)?;
    // Alias the bare local part of the owning creature's username → its id, so a
    // request may address the route by the short name (`/m-tool-github/…`) as
    // well as by the full username or the numeric id. Best-effort: only when the
    // creature record + username resolve on this node.
    let username = (crate::api::model::creature_ports::CreaturePorts { trx })
        .creature_or_empty(creature_id)
        .username;
    let local_part = crate::adapters::vmm::http_route::username_local_part(&username);
    if !local_part.is_empty() && local_part != creature_id {
        routes.put_alias(local_part, creature_id).map_err(failed)?;
    }
    Ok(())
}

fn deploy(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    served::<DeployInput>(app, "/programs/deploy", serve_deploy_entity)
}

/// `/programs/downloadEntity` — hand a deployed downloadable entity's file to
/// the caller (base64). This is how front-end apps deployed as entities are
/// fetched and executed on the client side at any time.
fn download_entity(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    served::<DownloadEntityInput>(app, "/programs/downloadEntity", serve_download_entity)
}

fn list_machines(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ListInput, _>(
        app,
        "/machines/list",
        user_guard(),
        move |state: Arc<dyn IState>, input: ListInput| -> Result<Value> {
            let trx = state.trx();
            // "Machines" are just creatures of type "machine".
            let creatures = crate::api::model::creature_ports::CreaturePorts { trx: &trx };
            let machines = aseman_application::creature::GetCreature {
                directory: &creatures,
                balances: &creatures,
            }
            .list(Some("machine"), input.offset, Some(input.count))
            .map_err(crate::api::model::store_ports::legacy_error)?
            .into_iter()
            .map(|found| {
                crate::api::model::creature_ports::creature_view(found.record, found.balance)
            });
            let mut result: Vec<Map<String, Value>> = Vec::new();
            for machine in machines {
                let profile = creatures.metadata_object(
                    aseman_domain::creature::MetadataKind::Creature,
                    &machine.id,
                    "metadata.public.profile",
                );
                let mut row: Map<String, Value> = Map::new();
                row.insert("id".into(), json!(machine.id));
                row.insert("chainId".into(), json!(machine.chain_id));
                row.insert("username".into(), json!(machine.username));
                row.insert("ownerId".into(), json!(machine.owner_id));
                row.insert("programsCount".into(), json!(machine.machines_count));
                if let Some(p) = profile {
                    row.insert(
                        "title".into(),
                        p.get("title").cloned().unwrap_or_else(|| json!("untitled")),
                    );
                    row.insert(
                        "avatar".into(),
                        p.get("avatar").cloned().unwrap_or_else(|| json!("")),
                    );
                    row.insert(
                        "desc".into(),
                        p.get("desc").cloned().unwrap_or_else(|| json!("")),
                    );
                } else {
                    row.insert("title".into(), json!("untitled"));
                    row.insert("avatar".into(), json!(""));
                    row.insert("desc".into(), json!(""));
                }
                result.push(row);
            }
            Ok(json!({"machines": result}))
        },
    )
}

fn list_programs(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    served::<ListInput>(app, "/programs/list", serve_list_programs)
}

fn list_program_machines(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    served::<ListAppMachsInput>(
        app,
        "/machines/listProgramMachines",
        serve_list_program_machines,
    )
}

/// Mirror of Go's `Install`: walk the existing programs, hand each one to the
/// VMM and replay any pending vm-alarm. The 15-second billing ticker is
/// intentionally not started here — see the module doc-comment.
fn install_program_bootstrap(app: Arc<dyn ICore>) {
    let app_for_closure = app.clone();
    app.modify_state(
        true,
        Box::new(move |trx: &Trx| {
            let programs = aseman_ports::ProgramDirectory::programs(
                &crate::api::model::program_ports::ProgramPorts { trx },
                0,
                None,
            )
            .map_err(|error| anyhow!("{error}"))?
            .into_iter()
            .map(crate::api::model::program_ports::program_view)
            .collect::<Vec<_>>();
            for program in programs {
                let is_proxy = normalize_entity_type(&program.runtime)
                    == crate::adapters::vmm::proxy::PROXY_RUNTIME_KEY;
                let is_vm = crate::api::workloads::remote()
                    .is_some_and(|remote| remote.offers(&program.runtime));
                // Proxy programs are non-runnable, but their signal listener must
                // still be re-registered on restart so forwarded prompts reach
                // them; only real VM runtimes additionally replay a pending alarm.
                if is_proxy || is_vm {
                    app_for_closure.tools().workloads().assign(&program.id);
                }
                if is_vm {
                    let pending = aseman_ports::ProgramAlarms::alarm(
                        &crate::api::model::program_ports::ProgramPorts { trx },
                        &program.id,
                    )
                    .map_err(|error| anyhow!("{error}"))?;
                    if let Some(alarm) = pending {
                        let app_async = app_for_closure.clone();
                        let machine_id = program.id.clone();
                        let store_id_clone = alarm.store_id.clone();
                        let alarm_data = alarm.data.clone();
                        // Older alarms without an entity replay "main" (the port's
                        // legacy default), so a wasm creature's module still resolves.
                        let alarm_entity = alarm.entity.clone();
                        let _ = async_once(move || {
                            let t = alarm.fire_at_millis;
                            let ct = chrono::Utc::now().timestamp_millis();
                            if t > ct {
                                std::thread::sleep(std::time::Duration::from_millis(
                                    (t - ct) as u64,
                                ));
                            }
                            // Note: the original Go path cleared the alarm
                            // links inside a state-modifying closure; here we
                            // call into vmm directly. The links are reaped on
                            // the next bootstrap pass if they remain stale.
                            if app_async
                                .tools()
                                .security()
                                .has_access_to_store(&machine_id, &store_id_clone)
                            {
                                app_async.tools().workloads().run_vm_entity(
                                    &machine_id,
                                    &store_id_clone,
                                    &alarm_data,
                                    &alarm_entity,
                                );
                            }
                        });
                    }
                }
                let ports = crate::api::model::store_ports::MembershipPorts { trx };
                let store_ids =
                    aseman_ports::StoreAccess::stores_of(&ports, &program.id).unwrap_or_default();
                for bare in store_ids {
                    app_for_closure
                        .tools()
                        .signaler()
                        .join_group(&bare, &program.id);
                }
            }
            Ok(())
        }),
    );
}

/// Plug every program action into the actor.
pub fn install(app: Arc<dyn ICore>) {
    let actor = app.actor();
    let handlers: Vec<Arc<dyn ISecureAction>> = vec![
        create_program(app.clone()),
        delete_program(app.clone()),
        update_program(app.clone()),
        run_program_entity(app.clone()),
        stop_program_entity(app.clone()),
        delete_program_entity(app.clone()),
        list_entity_vms(app.clone()),
        read_vm_logs(app.clone()),
        open_vm_terminal(app.clone()),
        close_vm_terminal(app.clone()),
        read_machine_builds(app.clone()),
        deploy(app.clone()),
        download_entity(app.clone()),
        list_machines(app.clone()),
        list_programs(app.clone()),
        list_program_machines(app.clone()),
    ];
    for h in handlers {
        actor.inject_secure_action(h);
    }
    install_program_bootstrap(app.clone());
    // Reap proxy-entity correlation records whose response never arrived,
    // so silent target failures cannot leak records into the database.
    crate::adapters::vmm::proxy::start_correlation_reaper(app.clone());
    let billing_lock = Arc::new(Mutex::new(-1i64));
    let app_bg = app.clone();
    std::thread::spawn(move || {
        loop {
            charge_running_standalone_vms_if_needed(&app_bg, &billing_lock);
            std::thread::sleep(std::time::Duration::from_secs(15));
        }
    });
}

// ── Public-executor entry points (RL-004) ─────────────────────────────────────
// The entity/workload families are driver-coupled (VMM client, blob store, gateway
// routes, cluster). The public executor wires them through these bodies, which reuse
// the exact legacy handler logic with the caller's id resolved by the executor.

/// `/programs/deploy` (`entity.deploy`) body.
pub(crate) fn serve_deploy_entity(
    app: &Arc<dyn ICore>,
    trx: &Trx,
    user_id: &str,
    input: DeployInput,
) -> Result<Value> {
    let program_id = input.machine_id.clone();
    if aseman_ports::ProgramDirectory::program(
        &crate::api::model::program_ports::ProgramPorts { trx },
        &program_id,
    )
    .map_err(|error| anyhow!("{error}"))?
    .is_none()
    {
        return Err(anyhow!("program not found"));
    }
    let program = (crate::api::model::program_ports::ProgramPorts { trx })
        .program_or_empty(&program_id.clone());
    let owner_machine = resolve_program_owner_machine(trx, &program);
    if owner_machine.owner_id != user_id {
        return Err(anyhow!("access to vm denied"));
    }
    let entity_type = normalize_entity_type(&input.entity_type);
    if entity_type == crate::adapters::vmm::proxy::PROXY_RUNTIME_KEY {
        let config =
            crate::adapters::vmm::proxy::config_from_metadata(|k| input.metadata.get(k).cloned())
                .map_err(|e| anyhow!(e))?;
        let data = base64::engine::general_purpose::STANDARD
            .decode(&input.payload)
            .map_err(|e| anyhow!("{}", e))?;
        let blobs = crate::adapters::blob_store::node_blobs(&*app.tools().storage());
        let evidence = blobs.put_entity_file(&program.id, &input.entity_id, "proxy.data", &data)?;
        crate::adapters::vmm::proxy::record_proxy_entity(
            trx,
            &program.id,
            &input.entity_id,
            &evidence,
            &config,
        )?;
        app.tools().workloads().assign(&program.id);
        return Ok(json!({
            "proxy": true,
            "entityId": input.entity_id,
            "entityType": crate::adapters::vmm::proxy::PROXY_RUNTIME_KEY,
            "target": config.to_value(),
        }));
    }
    let remote = crate::api::workloads::remote()
        .ok_or_else(|| anyhow!("this node has no VMM (ASEMAN_VMM_ENDPOINT)"))?;
    let conventions = remote.deploy_conventions(&entity_type).ok_or_else(|| {
        anyhow!(
            "invalid entityType, expected one of {}",
            remote.runtime_keys().join("|")
        )
    })?;
    let primary_file_name = conventions.entity_file_name.clone();
    let accepts_extra_files = conventions.accepts_extra_files;
    let data = base64::engine::general_purpose::STANDARD
        .decode(&input.payload)
        .map_err(|e| anyhow!("{}", e))?;
    let distributed = input.wants_distribution();
    let distribution_label = if distributed { "cluster" } else { "local" };
    let blobs = crate::adapters::blob_store::node_blobs(&*app.tools().storage());
    let primary =
        blobs.put_entity_file(&program.id, &input.entity_id, &primary_file_name, &data)?;
    if accepts_extra_files {
        let mut files: HashMap<String, Value> = HashMap::new();
        if let Some(files_raw) = input.metadata.get("files") {
            match files_raw {
                Value::Object(o) => {
                    files = o.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                }
                Value::Null => {}
                _ => return Err(anyhow!("files is not map")),
            }
        }
        for (k, v) in &files {
            let data_str = match v {
                Value::String(s) => s.clone(),
                _ => return Err(anyhow!("file bytecode not string")),
            };
            let raw = base64::engine::general_purpose::STANDARD
                .decode(&data_str)
                .map_err(|e| anyhow!("{}", e))?;
            blobs.put_entity_file(&program.id, &input.entity_id, k, &raw)?;
        }
    }
    let gateway_path = crate::adapters::vmm::http_route::normalize_path(
        input
            .metadata
            .get("gatewayPath")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    let gateway_vm_id = input
        .metadata
        .get("gatewayVmId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    register_gateway_route(
        trx,
        &program.machine_id,
        &program.id,
        &input.entity_id,
        &gateway_path,
        &gateway_vm_id,
        &entity_type,
    )?;
    app.tools().workloads().assign(&program.id);
    aseman_application::program::RecordEntityDeployment {
        entities: &EntityPorts { trx },
    }
    .execute(&aseman_application::program::EntityDeployment {
        entity: EntityRecord {
            program_id: program.id.clone(),
            entity_id: input.entity_id.clone(),
            entity_type: entity_type.clone(),
            image_name: input.entity_id.clone(),
        },
        primary,
        runtime_file: true,
        downloadable: input.downloadable,
        config: None,
    })
    .map_err(|error| anyhow!("{error}"))?;
    vm_runtime::set_distribution(trx, &program.id, None, distribution_label)?;
    vm_runtime::set_distribution(trx, &program.id, Some(&input.entity_id), distribution_label)?;
    let mut result = serde_json::to_value(PlugInput::default())?;
    if let Value::Object(map) = &mut result {
        map.insert("distribution".into(), json!(distribution_label));
    }
    Ok(result)
}

/// `/programs/downloadEntity` (`entity.download`) body.
pub(crate) fn serve_download_entity(
    app: &Arc<dyn ICore>,
    trx: &Trx,
    _user_id: &str,
    input: DownloadEntityInput,
) -> Result<Value> {
    let program_id = if input.program_id.is_empty() {
        input.machine_id.clone()
    } else {
        input.program_id.clone()
    };
    if program_id.is_empty() || input.entity_id.is_empty() {
        return Err(anyhow!("programId and entityId are required"));
    }
    let blobs = crate::adapters::blob_store::node_blobs(&*app.tools().storage());
    let entities = EntityPorts { trx };
    let artifact = entities
        .artifact(&program_id, &input.entity_id, ArtifactRole::Downloadable)
        .map_err(|error| anyhow!("{error}"))?
        .ok_or_else(|| anyhow!("entity is not downloadable"))?;
    let entity = entities
        .entity(&program_id, &input.entity_id)
        .map_err(|error| anyhow!("{error}"))?
        .unwrap_or_default();
    let bytes = artifact
        .store_key
        .and_then(|key| blobs.blob(&key).ok().flatten())
        .ok_or_else(|| anyhow!("entity file unavailable"))?;
    Ok(json!({
        "programId": program_id,
        "entityId": input.entity_id,
        "entityType": entity.entity_type,
        "payload": base64::engine::general_purpose::STANDARD.encode(bytes),
    }))
}

/// `/programs/runEntity` (`workload.start`) body.
pub(crate) fn serve_run_program_entity(
    app: &Arc<dyn ICore>,
    trx: &Trx,
    user_id: &str,
    input: RunProgramEntityInput,
) -> Result<Value> {
    let program_id = if input.program_id.is_empty() {
        input.machine_id.clone()
    } else {
        input.program_id.clone()
    };
    if aseman_ports::ProgramDirectory::program(
        &crate::api::model::program_ports::ProgramPorts { trx },
        &program_id,
    )
    .map_err(|error| anyhow!("{error}"))?
    .is_none()
    {
        return Err(anyhow!("program does not exist"));
    }
    let program = (crate::api::model::program_ports::ProgramPorts { trx })
        .program_or_empty(&program_id.clone());
    let entity = read_entity(trx, &program.id, &input.entity_id)?
        .ok_or_else(|| anyhow!("entity does not exist"))?;
    let entity_type = normalize_entity_type(&entity.entity_type);
    let owner_machine = resolve_program_owner_machine(trx, &program);
    if owner_machine.owner_id != user_id {
        return Err(anyhow!("you are not owner of this program"));
    }
    let vm_id = Uuid::new_v4().to_string();
    let started_at_millis = chrono::Utc::now().timestamp_millis();
    let gateway_path = crate::adapters::vmm::http_route::normalize_path(&input.gateway_path);
    if !gateway_path.is_empty() {
        register_gateway_route(
            trx,
            &owner_machine.id,
            &program.id,
            &input.entity_id,
            &gateway_path,
            &vm_id,
            &entity_type,
        )?;
    }
    let distributed = vm_runtime::distribution(trx, &program.id, None)? == "cluster"
        || vm_runtime::distribution(trx, &program.id, Some(&input.entity_id))? == "cluster";
    let mut billing = None;
    let resources = normalize_vm_resources(&input.resources);
    let vm_is_free = crate::api::actions::program::vm_is_free(app);
    if !vm_is_free {
        let mut billing_data = validate_and_build_vm_billing(
            app,
            trx,
            user_id,
            &input.payment_lock_id,
            &input.payment_signatures,
            &resources,
        )?;
        billing_data.insert("machineId".into(), json!(input.machine_id));
        billing_data.insert("entityId".into(), json!(input.entity_id));
        billing_data.insert("vmId".into(), json!(vm_id));
        billing = Some(billing_data);
    }
    let remote = crate::api::workloads::remote()
        .ok_or_else(|| anyhow!("this node has no VMM (ASEMAN_VMM_ENDPOINT)"))?;
    if !remote.offers(&entity_type) {
        return Err(anyhow!("invalid entity type"));
    }
    let params: HashMap<String, String> = if input.params.is_empty() {
        HashMap::new()
    } else {
        input.params.clone()
    };
    vm_runtime::record_launch(
        trx,
        &vm_runtime::Launch {
            vm_id: &vm_id,
            program_id: &program.id,
            entity_id: &input.entity_id,
            started_at_millis,
            distributed,
            billing,
        },
    )?;
    remote.launch(
        &program.id,
        &program.machine_id,
        &input.entity_id,
        &vm_id,
        &entity_type,
        crate::api::workloads::LaunchResources {
            cpu_cores: resources.cpu_cores,
            ram_mb: resources.ram_mb,
            disk_gb: resources.disk_gb,
            max_exec_time_seconds: resources.max_exec_time_seconds,
        },
        params.into_iter().collect(),
    )?;
    Ok(json!({"vmId": vm_id}))
}

/// `/programs/stopEntity` (`workload.stop`) body.
pub(crate) fn serve_stop_program_entity(
    _app: &Arc<dyn ICore>,
    trx: &Trx,
    user_id: &str,
    input: RunProgramEntityInput,
) -> Result<Value> {
    let program_id = if input.program_id.is_empty() {
        input.machine_id.clone()
    } else {
        input.program_id.clone()
    };
    if aseman_ports::ProgramDirectory::program(
        &crate::api::model::program_ports::ProgramPorts { trx },
        &program_id,
    )
    .map_err(|error| anyhow!("{error}"))?
    .is_none()
    {
        return Err(anyhow!("program does not exist"));
    }
    let program = (crate::api::model::program_ports::ProgramPorts { trx })
        .program_or_empty(&program_id.clone());
    read_entity(trx, &program.id, &input.entity_id)?
        .ok_or_else(|| anyhow!("entity does not exist"))?;
    let owner_machine = resolve_program_owner_machine(trx, &program);
    if owner_machine.owner_id != user_id {
        return Err(anyhow!("you are not owner of this program"));
    }
    let vm_id = input.vm_id.clone();
    vm_runtime::mark_stopped(trx, &vm_id)?;
    crate::api::workloads::remote()
        .ok_or_else(|| anyhow!("this node has no VMM (ASEMAN_VMM_ENDPOINT)"))?
        .set_state(
            user_id,
            crate::api::workloads::RemoteWorkloads::workload_id(
                &program.id,
                &input.entity_id,
                &vm_id,
            ),
            aseman_domain::DesiredWorkloadState::Stopped,
        )?;
    Ok(json!({}))
}

/// `/programs/deleteEntity` (`entity.delete`) body.
pub(crate) fn serve_delete_program_entity(
    _app: &Arc<dyn ICore>,
    trx: &Trx,
    user_id: &str,
    input: RunProgramEntityInput,
) -> Result<Value> {
    let program_id = if input.program_id.is_empty() {
        input.machine_id.clone()
    } else {
        input.program_id.clone()
    };
    if aseman_ports::ProgramDirectory::program(
        &crate::api::model::program_ports::ProgramPorts { trx },
        &program_id,
    )
    .map_err(|error| anyhow!("{error}"))?
    .is_none()
    {
        return Err(anyhow!("program does not exist"));
    }
    let program = (crate::api::model::program_ports::ProgramPorts { trx })
        .program_or_empty(&program_id.clone());
    read_entity(trx, &program.id, &input.entity_id)?
        .ok_or_else(|| anyhow!("entity does not exist"))?;
    let owner_machine = resolve_program_owner_machine(trx, &program);
    if owner_machine.owner_id != user_id {
        return Err(anyhow!("you are not owner of this program"));
    }
    let vm_id = input.vm_id.trim().to_string();
    if vm_id.is_empty() {
        return Err(anyhow!("vmId is required"));
    }
    let belongs = vm_runtime::instance(trx, &vm_id)?.is_some_and(|instance| {
        instance.program_ref.as_deref() == Some(program.id.as_str())
            && instance.entity_ref.as_deref() == Some(input.entity_id.as_str())
    });
    if !belongs {
        return Err(anyhow!("vm does not belong to this entity"));
    }
    let generation = crate::api::workloads::remote()
        .ok_or_else(|| anyhow!("this node has no VMM (ASEMAN_VMM_ENDPOINT)"))?
        .set_state(
            user_id,
            crate::api::workloads::RemoteWorkloads::workload_id(
                &program.id,
                &input.entity_id,
                &vm_id,
            ),
            aseman_domain::DesiredWorkloadState::Deleted,
        )?;
    let result = json!({"ok": true, "generation": generation});
    if !result["ok"].as_bool().unwrap_or(false) {
        let err = result["error"]
            .as_str()
            .unwrap_or("vm delete failed")
            .to_string();
        return Err(anyhow!(err));
    }
    vm_runtime::forget(trx, &vm_id)?;
    Ok(json!({"ok": true, "vmId": vm_id, "result": result}))
}

/// `/machines/listEntityVms` (`workload.list`) body.
pub(crate) fn serve_list_entity_vms(
    app: &Arc<dyn ICore>,
    trx: &Trx,
    user_id: &str,
    input: RunProgramEntityInput,
) -> Result<Value> {
    let program_id = if input.program_id.is_empty() {
        input.machine_id.clone()
    } else {
        input.program_id.clone()
    };
    if aseman_ports::ProgramDirectory::program(
        &crate::api::model::program_ports::ProgramPorts { trx },
        &program_id,
    )
    .map_err(|error| anyhow!("{error}"))?
    .is_none()
    {
        return Err(anyhow!("program does not exist"));
    }
    let program = (crate::api::model::program_ports::ProgramPorts { trx })
        .program_or_empty(&program_id.clone());
    let owner_machine = resolve_program_owner_machine(trx, &program);
    if owner_machine.owner_id != user_id {
        return Err(anyhow!("you are not owner of this program"));
    }
    let entity = read_entity(trx, &program.id, &input.entity_id)?
        .ok_or_else(|| anyhow!("entity does not exist"))?;
    let entity_type = normalize_entity_type(&entity.entity_type);
    let remote = crate::api::workloads::remote()
        .ok_or_else(|| anyhow!("this node has no VMM (ASEMAN_VMM_ENDPOINT)"))?;
    let mut instances: Vec<Value> = Vec::new();
    for instance in vm_runtime::instances_of(trx, &program.id, Some(&input.entity_id))? {
        let vm_id = instance.key.clone();
        let recorded_status = instance.status.clone().unwrap_or_default();
        let started_at = instance.started_at_millis.unwrap_or(0);
        let probe: Result<Value, String> = serde_json::from_str::<Value>(&remote.vm_host_call(
            app,
            "statusVm",
            &program.id,
            &json!({
                "runtime": entity_type,
                "machineId": program.id,
                "entityId": input.entity_id,
                "vmId": vm_id,
            }),
        ))
        .map_err(|error| error.to_string())
        .and_then(|value| {
            if value["ok"] == false {
                Err(value["error"].as_str().unwrap_or("unknown").to_owned())
            } else {
                Ok(value)
            }
        });
        let (status, running, detail) = match probe {
            Ok(value) => {
                let status = value["status"].as_str().unwrap_or("unknown").to_string();
                let running = value["running"].as_bool().unwrap_or(status == "running");
                (status, running, value)
            }
            Err(error) => (
                if recorded_status.is_empty() {
                    "stopped"
                } else {
                    "unknown"
                }
                .to_string(),
                false,
                json!({"error": error}),
            ),
        };
        instances.push(json!({
            "vmId": vm_id,
            "status": status,
            "running": running,
            "recordedStatus": recorded_status,
            "startedAt": started_at,
            "detail": detail,
        }));
    }
    instances.sort_by(|a, b| {
        b["startedAt"]
            .as_i64()
            .unwrap_or(0)
            .cmp(&a["startedAt"].as_i64().unwrap_or(0))
    });
    Ok(json!({
        "programId": program.id,
        "entityId": input.entity_id,
        "runtime": entity_type,
        "instances": instances,
    }))
}

/// `/machines/readVmLogs` (`workload.logs.read`) body.
pub(crate) fn serve_read_vm_logs(
    _app: &Arc<dyn ICore>,
    trx: &Trx,
    user_id: &str,
    input: ReadVmLogsInput,
) -> Result<Value> {
    let instance = vm_runtime::instance(trx, &input.vm_id)?
        .and_then(|instance| Some((instance.program_ref?, instance.entity_ref?)));
    let Some((program_id, entity_id)) = instance else {
        return Err(anyhow!("vm not found"));
    };
    let program =
        (crate::api::model::program_ports::ProgramPorts { trx }).program_or_empty(&program_id);
    if program.id.is_empty() {
        return Err(anyhow!("vm not found"));
    }
    if resolve_program_owner_machine(trx, &program).owner_id != user_id {
        return Err(anyhow!("you are not owner of this vm"));
    }
    let remote = crate::api::workloads::remote()
        .ok_or_else(|| anyhow!("this node has no VMM (ASEMAN_VMM_ENDPOINT)"))?;
    let count = usize::try_from(input.count).unwrap_or(0).clamp(1, 1000);
    let after = u64::try_from(input.offset).unwrap_or(0);
    let build = input.log_type == "build";
    let workload_id =
        crate::api::workloads::RemoteWorkloads::workload_id(&program.id, &entity_id, &input.vm_id);
    let logs: Vec<Value> = remote
        .logs(workload_id, after)?
        .into_iter()
        .filter(|record| (record.stream == aseman_domain::vmm::LogStream::Build) == build)
        .take(count)
        .map(|record| {
            serde_json::to_value(crate::models::packet::BuildPacket {
                id: record.sequence.to_string(),
                build_id: input.vm_id.clone(),
                creature_id: program.machine_id.clone(),
                vm_id: input.vm_id.clone(),
                log_type: input.log_type.clone(),
                time: record.at_millis,
                data: record.line,
            })
            .unwrap_or_default()
        })
        .collect();
    Ok(json!({"logs": logs, "workloadId": workload_id.to_string()}))
}

/// `/machines/openVmTerminal` (`workload.terminal.open`) body.
pub(crate) fn serve_open_vm_terminal(
    _app: &Arc<dyn ICore>,
    trx: &Trx,
    user_id: &str,
    input: VmTerminalInput,
) -> Result<Value> {
    if aseman_ports::ProgramDirectory::program(
        &crate::api::model::program_ports::ProgramPorts { trx },
        &input.creature_id,
    )
    .map_err(|error| anyhow!("{error}"))?
    .is_none()
    {
        return Err(anyhow!("program does not exist"));
    }
    let program = (crate::api::model::program_ports::ProgramPorts { trx })
        .program_or_empty(&input.creature_id.clone());
    let owner_machine = resolve_program_owner_machine(trx, &program);
    if owner_machine.owner_id != user_id {
        return Err(anyhow!("you are not owner of this creature"));
    }
    vm_runtime::set_terminal(trx, &input.creature_id, &input.vm_id, user_id, true)?;
    Ok(json!({"terminal": "on"}))
}

/// `/machines/closeVmTerminal` (`workload.terminal.close`) body.
pub(crate) fn serve_close_vm_terminal(
    _app: &Arc<dyn ICore>,
    trx: &Trx,
    user_id: &str,
    input: VmTerminalInput,
) -> Result<Value> {
    if aseman_ports::ProgramDirectory::program(
        &crate::api::model::program_ports::ProgramPorts { trx },
        &input.creature_id,
    )
    .map_err(|error| anyhow!("{error}"))?
    .is_none()
    {
        return Err(anyhow!("program does not exist"));
    }
    let program = (crate::api::model::program_ports::ProgramPorts { trx })
        .program_or_empty(&input.creature_id.clone());
    let owner_machine = resolve_program_owner_machine(trx, &program);
    if owner_machine.owner_id != user_id {
        return Err(anyhow!("you are not owner of this creature"));
    }
    vm_runtime::set_terminal(trx, &input.creature_id, &input.vm_id, user_id, false)?;
    Ok(json!({"terminal": "off"}))
}

/// `/machines/readMachineBuilds` (`workload.builds.read`) body.
pub(crate) fn serve_read_machine_builds(
    _app: &Arc<dyn ICore>,
    trx: &Trx,
    _user_id: &str,
    input: MachineBuildsInput,
) -> Result<Value> {
    // No runtime records build lists any more (the VMM owns builds, ADR 0022).
    let _ = (trx, input);
    Ok(json!({"buildsList": Vec::<String>::new()}))
}

/// `/programs/list` (`program.list`) body.
pub(crate) fn serve_list_programs(
    _app: &Arc<dyn ICore>,
    trx: &Trx,
    _user_id: &str,
    input: ListInput,
) -> Result<Value> {
    let count = (input.count != -1).then_some(input.count);
    let machines = aseman_ports::ProgramDirectory::programs(
        &crate::api::model::program_ports::ProgramPorts { trx },
        input.offset,
        count,
    )
    .map_err(|error| anyhow!("{error}"))?
    .into_iter()
    .map(crate::api::model::program_ports::program_view)
    .collect::<Vec<_>>();
    Ok(json!({"machines": machines}))
}

/// `/machines/listProgramMachines` (`program.list`) body.
pub(crate) fn serve_list_program_machines(
    _app: &Arc<dyn ICore>,
    trx: &Trx,
    _user_id: &str,
    input: ListAppMachsInput,
) -> Result<Value> {
    let programs = aseman_ports::ProgramDirectory::programs_of_machine(
        &crate::api::model::program_ports::ProgramPorts { trx },
        &input.app_id,
    )
    .map_err(|error| anyhow!("{error}"))?
    .into_iter()
    .map(crate::api::model::program_ports::program_view)
    .collect::<Vec<_>>();
    let users = programs
        .iter()
        .filter_map(|program| {
            aseman_ports::CreatureDirectory::creature(
                &crate::api::model::creature_ports::CreaturePorts { trx },
                &program.id,
            )
            .ok()
            .flatten()
        })
        .map(|record| crate::api::model::creature_ports::creature_view(record, 0))
        .collect::<Vec<_>>();
    let mut program_by_machine_id: HashMap<String, Program> = HashMap::new();
    for program in programs {
        program_by_machine_id.insert(program.id.clone(), program);
    }
    let mut result: Vec<Map<String, Value>> = Vec::new();
    for user in users {
        let mut row: Map<String, Value> = Map::new();
        row.insert("id".into(), json!(user.id));
        row.insert("type".into(), json!(user.type_name));
        row.insert("username".into(), json!(user.username));
        let comment = program_by_machine_id
            .get(&user.id)
            .map(|p| p.comment.clone())
            .unwrap_or_default();
        row.insert("comment".into(), json!(comment));
        result.push(row);
    }
    Ok(json!({"machines": result}))
}
