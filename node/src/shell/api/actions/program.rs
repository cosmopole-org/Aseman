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

use anyhow::{anyhow, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::core::actor::model::secured::guard::Guard;
use crate::models::action::ISecureAction;
use crate::models::core::ICore;
use crate::models::state::IState;
use crate::models::transaction::ITrx;
use crate::shell::api::model::entity_ports::EntityPorts;
use crate::shell::api::model::{Creature, Program};
use crate::shell::api::packets::plugin::PlugInput;
use crate::shell::api::packets::program::{
    CreateMachineInput, DeleteProgramInput, DeployInput, DownloadEntityInput, ListAppMachsInput,
    ListInput, MachineBuildsInput, ReadVmLogsInput, RunProgramEntityInput, UpdateProgramInput,
    VmResourcesInput, VmTerminalInput,
};
use crate::shell::utils::future::async_once;
use aseman_domain::program::{ArtifactRole, EntityRecord};
use aseman_ports::{BlobStore, EntityDirectory};

use super::util::build_secure_action;

const PLUGINS_TEMPLATE_NAME: &str = "/machines/";

fn user_guard() -> Guard {
    Guard {
        is_user: true,
        is_in_store: false,
        allow_applet_sign: true,
    }
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
pub(crate) fn resolve_program_owner_machine(trx: &dyn ITrx, program: &Program) -> Creature {
    let canonical = (crate::shell::api::model::creature_ports::CreaturePorts { trx })
        .creature_or_empty(&program.machine_id.clone());
    if !canonical.owner_id.is_empty() {
        return canonical;
    }

    // The derived `machinePrograms` link always names `program.machine_id` (the
    // legacy adapter maintains it, and the A308 export verifies it), so a reverse
    // scan cannot find another owner.
    Creature::default()
}

fn as_i64(raw: &Value) -> Option<i64> {
    match raw {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct VmResources {
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
    if cost <= 0 {
        1
    } else {
        cost
    }
}

fn validate_and_build_vm_billing(
    app: &Arc<dyn ICore>,
    trx: &dyn ITrx,
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
    let payment = trx
        .get_json(
            &format!("Json::Creature::{}", payer_id),
            &format!("lockedTokens.{}", lock_id),
        )
        .map_err(|_| anyhow!("payment lock not found"))?;
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

/// Helper that walks the running `VmBilling::*` link space and produces the
/// list of charge targets the per-minute ticker would normally consume.
///
/// TODO: the timed scheduler is intentionally not started by `install`. Until
/// it is, this helper is unreachable; it's preserved so the scheduler can be
/// dropped in without re-translating the billing logic.
#[allow(dead_code)]
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
        Box::new(move |tx: &dyn ITrx| {
            let links = match tx.get_links_list("VmBilling::", -1, -1, &[]) {
                Ok(v) => v,
                Err(_) => return Ok(()),
            };
            let mut acc = targets_for_closure.lock().unwrap();
            for link in links {
                let vm_id = link.trim_start_matches("VmBilling::").to_string();
                if vm_id.is_empty() || tx.get_link(&format!("VmStatus::{}", vm_id)) != "running" {
                    continue;
                }
                let billing = match tx.get_json(&format!("Json::VmBilling::{}", vm_id), "payment") {
                    Ok(b) => b,
                    Err(_) => continue,
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
                Box::new(move |tx: &dyn ITrx| {
                    tx.del_key(&format!("link::VmStatus::{}", vm_id_for_closure));
                    tx.del_key(&format!(
                        "link::VmInstance::{}::{}::{}",
                        machine_id_for_closure, entity_id_for_closure, vm_id_for_closure
                    ));
                    tx.del_key(&format!("link::VmBilling::{}", vm_id_for_closure));
                    tx.del_json(
                        &format!("Json::VmBilling::{}", vm_id_for_closure),
                        "payment",
                    );
                    Ok(())
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
                Box::new(move |tx: &dyn ITrx| {
                    let mut billing = tx
                        .get_json(&format!("Json::VmBilling::{}", vm_for_closure), "payment")
                        .unwrap_or_default();
                    let current_step = billing.get("currentStep").and_then(as_i64).unwrap_or(0);
                    billing.insert("currentStep".into(), json!(current_step + 1));
                    billing.insert("lastChargeMinute".into(), json!(current_minute));
                    tx.put_json(
                        &format!("Json::VmBilling::{}", vm_for_closure),
                        "payment",
                        &Value::Object(billing),
                        true,
                    )?;
                    Ok(())
                }),
            );
        } else {
            terminate_standalone_vm(app, &machine_id, &entity_id, &vm_id);
        }
    }
    *guard = current_minute;
}

/// A program's entity, read through the entity port.
fn read_entity(
    app: &Arc<dyn ICore>,
    trx: &dyn ITrx,
    program_id: &str,
    entity_id: &str,
) -> Result<Option<EntityRecord>> {
    let blobs = crate::drivers::blob_store::node_blobs(&*app.tools().storage());
    EntityPorts { trx, blobs: &blobs }
        .entity(program_id, entity_id)
        .map_err(|error| anyhow!("{error}"))
}

/// Stop one standalone instance (the billing reaper): a desired-state change made
/// as the program's owning creature, whose ownership the node established.
fn terminate_standalone_vm(app: &Arc<dyn ICore>, machine_id: &str, entity_id: &str, vm_id: &str) {
    let Some(remote) = crate::shell::workloads::remote() else {
        eprintln!("cannot stop {machine_id}/{entity_id}/{vm_id}: this node has no VMM");
        return;
    };
    let owner = crate::shell::workloads::program_machine(app, machine_id);
    if let Err(error) = remote.set_state_as(
        crate::shell::workloads::creature_subject(&owner),
        crate::shell::workloads::RemoteWorkloads::workload_id(machine_id, entity_id, vm_id),
        aseman_domain::DesiredWorkloadState::Stopped,
    ) {
        eprintln!("cannot stop {machine_id}/{entity_id}/{vm_id}: {error}");
    }
}

fn create_program(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<CreateMachineInput, _>(
        app,
        "/programs/create",
        user_guard(),
        move |state: Arc<dyn IState>, input: CreateMachineInput| -> Result<Value> {
            let trx = state.trx();
            let creatures = crate::shell::api::model::creature_ports::CreaturePorts { trx: &*trx };
            let programs = crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx };
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
                        .gen_id(&*trx, &crate::models::input::IInput::origin(&input)),
                    machine_id: input.app_id.clone(),
                    runtime: input.runtime.clone(),
                    path: input.path.clone(),
                    comment: input.comment.clone(),
                },
            )
            .map_err(crate::shell::api::model::store_ports::legacy_error)?;
            programs
                .merge_metadata_value(&created.id, &json!({}))
                .map_err(|error| anyhow!("{error}"))?;
            let program = crate::shell::api::model::program_ports::program_view(created);
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
            let programs = crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx };
            // LD-17: the program and its relation are really removed; LD-18: only the
            // owner of the program's machine may delete it.
            aseman_application::program::DeleteProgram {
                creatures: &crate::shell::api::model::creature_ports::CreaturePorts { trx: &*trx },
                programs: &programs,
            }
            .execute(&state.info().user_id(), &input.program_id)
            .map_err(crate::shell::api::model::store_ports::legacy_error)?;
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
            let programs = crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx };
            // LD-18: only the owner of the program's machine may change it.
            let program = aseman_application::program::UpdateProgramPath {
                creatures: &crate::shell::api::model::creature_ports::CreaturePorts { trx: &*trx },
                programs: &programs,
            }
            .execute(&state.info().user_id(), &input.program_id, &input.path)
            .map_err(crate::shell::api::model::store_ports::legacy_error)?;
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
    let app_for_handler = app.clone();
    build_secure_action::<RunProgramEntityInput, _>(
        app,
        "/programs/runEntity",
        user_guard(),
        move |state: Arc<dyn IState>, input: RunProgramEntityInput| -> Result<Value> {
            let trx = state.trx();
            let program_id = if input.program_id.is_empty() {
                input.machine_id.clone()
            } else {
                input.program_id.clone()
            };
            if aseman_ports::ProgramDirectory::program(
                &crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx },
                &program_id,
            )
            .map_err(|error| anyhow!("{error}"))?
            .is_none()
            {
                return Err(anyhow!("program does not exist"));
            }
            let program = (crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx })
                .program_or_empty(&program_id.clone());
            let entity = read_entity(&app_for_handler, &*trx, &program.id, &input.entity_id)?
                .ok_or_else(|| anyhow!("entity does not exist"))?;
            let entity_type = normalize_entity_type(&entity.entity_type);
            // A program owns itself: authorize against the recorded program owner
            // rather than the deprecated app_id parent pointer.
            let owner_machine = resolve_program_owner_machine(&*trx, &program);
            if owner_machine.owner_id != state.info().user_id() {
                return Err(anyhow!("you are not owner of this program"));
            }
            let vm_id = Uuid::new_v4().to_string();
            trx.put_link(&format!("VmStatus::{}", vm_id), "running");
            trx.put_link(
                &format!("VmStartedAt::{}", vm_id),
                &chrono::Utc::now().timestamp_millis().to_string(),
            );
            // Bind a deterministic custom VM gateway route to this specific
            // instance when requested. The external URL stays fixed across
            // redeploys (keyed by the owning creature's username + path); only
            // the route's target vm id is refreshed here to the fresh instance.
            let gateway_path = crate::drivers::vmm::http_route::normalize_path(&input.gateway_path);
            if !gateway_path.is_empty() {
                register_gateway_route(
                    &*trx,
                    &owner_machine.id,
                    &program.id,
                    &input.entity_id,
                    &gateway_path,
                    &vm_id,
                    &entity_type,
                )?;
            }
            // Tag VMs of cluster-distributed programs so their state commits
            // are propagated through the raft consensus (local-mode VMs are
            // deliberately left untagged and never enter the log).
            if trx.get_link(&format!("vmDistribution::{}", program.id)) == "cluster"
                || trx.get_link(&format!(
                    "vmDistribution::{}::{}",
                    program.id, input.entity_id
                )) == "cluster"
            {
                trx.put_link(&format!("vmDistributed::{}", vm_id), "true");
            }
            let resources = normalize_vm_resources(&input.resources);
            // Free-tier bypass: when every VM cost rate is zero there is nothing
            // to bill, so a payment lock is not required. Paid nodes still
            // enforce the lock + per-step signatures via validate_and_build_vm_billing.
            let vm_is_free = app_for_handler.vm_ram_cost_per_mb_per_minute() == 0
                && app_for_handler.vm_cpu_core_cost_per_minute() == 0
                && app_for_handler.vm_disk_cost_per_gb_per_minute() == 0;
            if !vm_is_free {
                let mut billing_data = validate_and_build_vm_billing(
                    &app_for_handler,
                    &*trx,
                    &state.info().user_id(),
                    &input.payment_lock_id,
                    &input.payment_signatures,
                    &resources,
                )?;
                billing_data.insert("machineId".into(), json!(input.machine_id));
                billing_data.insert("entityId".into(), json!(input.entity_id));
                billing_data.insert("vmId".into(), json!(vm_id));
                trx.put_link(&format!("VmBilling::{}", vm_id), "true");
                trx.put_json(
                    &format!("Json::VmBilling::{}", vm_id),
                    "payment",
                    &Value::Object(billing_data),
                    true,
                )?;
            }
            let remote = crate::shell::workloads::remote()
                .ok_or_else(|| anyhow!("this node has no VMM (ASEMAN_VMM_ENDPOINT)"))?;
            if !remote.offers(&entity_type) {
                return Err(anyhow!("invalid entity type"));
            }
            let params: HashMap<String, String> = if input.params.is_empty() {
                HashMap::new()
            } else {
                input.params.clone()
            };
            trx.put_link(
                &format!("VmInstance::{}::{}::{}", program.id, input.entity_id, vm_id),
                "true",
            );
            remote.launch(
                &program.id,
                &program.machine_id,
                &input.entity_id,
                &vm_id,
                &entity_type,
                crate::shell::workloads::LaunchResources {
                    cpu_cores: resources.cpu_cores,
                    ram_mb: resources.ram_mb,
                    disk_gb: resources.disk_gb,
                    max_exec_time_seconds: resources.max_exec_time_seconds,
                },
                params.into_iter().collect(),
            )?;
            Ok(json!({"vmId": vm_id}))
        },
    )
}

fn stop_program_entity(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<RunProgramEntityInput, _>(
        app,
        "/programs/stopEntity",
        user_guard(),
        move |state: Arc<dyn IState>, input: RunProgramEntityInput| -> Result<Value> {
            let trx = state.trx();
            let program_id = if input.program_id.is_empty() {
                input.machine_id.clone()
            } else {
                input.program_id.clone()
            };
            if aseman_ports::ProgramDirectory::program(
                &crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx },
                &program_id,
            )
            .map_err(|error| anyhow!("{error}"))?
            .is_none()
            {
                return Err(anyhow!("program does not exist"));
            }
            let program = (crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx })
                .program_or_empty(&program_id.clone());
            read_entity(&app_for_handler, &*trx, &program.id, &input.entity_id)?
                .ok_or_else(|| anyhow!("entity does not exist"))?;
            // Authorize against the recorded program owner (no app_id).
            let owner_machine = resolve_program_owner_machine(&*trx, &program);
            if owner_machine.owner_id != state.info().user_id() {
                return Err(anyhow!("you are not owner of this program"));
            }
            let vm_id = input.vm_id.clone();
            trx.del_key(&format!("link::VmStatus::{}", vm_id));
            trx.del_key(&format!("link::VmStartedAt::{}", vm_id));
            trx.del_key(&format!(
                "link::VmInstance::{}::{}::{}",
                program.id, input.entity_id, vm_id
            ));
            trx.del_key(&format!("link::VmBilling::{}", vm_id));
            trx.del_json(&format!("Json::VmBilling::{}", vm_id), "payment");
            trx.del_key(&format!(
                "link::vmStandaloneImageName::{}::{}",
                program.id, input.entity_id
            ));
            trx.del_key(&format!(
                "link::vmStandaloneContainerName::{}::{}",
                program.id, input.entity_id
            ));
            crate::shell::workloads::remote()
                .ok_or_else(|| anyhow!("this node has no VMM (ASEMAN_VMM_ENDPOINT)"))?
                .set_state(
                    &state.info().user_id(),
                    crate::shell::workloads::RemoteWorkloads::workload_id(
                        &program.id,
                        &input.entity_id,
                        &vm_id,
                    ),
                    aseman_domain::DesiredWorkloadState::Stopped,
                )?;
            Ok(json!({}))
        },
    )
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
    let app_for_handler = app.clone();
    build_secure_action::<RunProgramEntityInput, _>(
        app,
        "/programs/deleteEntity",
        user_guard(),
        move |state: Arc<dyn IState>, input: RunProgramEntityInput| -> Result<Value> {
            let trx = state.trx();
            let program_id = if input.program_id.is_empty() {
                input.machine_id.clone()
            } else {
                input.program_id.clone()
            };
            if aseman_ports::ProgramDirectory::program(
                &crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx },
                &program_id,
            )
            .map_err(|error| anyhow!("{error}"))?
            .is_none()
            {
                return Err(anyhow!("program does not exist"));
            }
            let program = (crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx })
                .program_or_empty(&program_id.clone());
            read_entity(&app_for_handler, &*trx, &program.id, &input.entity_id)?
                .ok_or_else(|| anyhow!("entity does not exist"))?;
            let owner_machine = resolve_program_owner_machine(&*trx, &program);
            if owner_machine.owner_id != state.info().user_id() {
                return Err(anyhow!("you are not owner of this program"));
            }
            let vm_id = input.vm_id.trim().to_string();
            if vm_id.is_empty() {
                return Err(anyhow!("vmId is required"));
            }
            // The instance must belong to THIS entity. Without the check a
            // caller who owns one program could pass any vm id and have the
            // runtime destroy an instance that program never launched.
            let instance_key =
                format!("VmInstance::{}::{}::{}", program.id, input.entity_id, vm_id);
            if trx.get_link(&instance_key).is_empty() {
                return Err(anyhow!("vm does not belong to this entity"));
            }

            let generation = crate::shell::workloads::remote()
                .ok_or_else(|| anyhow!("this node has no VMM (ASEMAN_VMM_ENDPOINT)"))?
                .set_state(
                    &state.info().user_id(),
                    crate::shell::workloads::RemoteWorkloads::workload_id(
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

            // The runtime destroyed it, so forget it. Doing this only after a
            // successful delete keeps a failed destroy visible (and retryable)
            // instead of leaving a live VM with no record.
            trx.del_key(&format!("link::VmStatus::{}", vm_id));
            trx.del_key(&format!("link::VmStartedAt::{}", vm_id));
            trx.del_key(&instance_key);
            trx.del_key(&format!("link::VmBilling::{}", vm_id));
            trx.del_json(&format!("Json::VmBilling::{}", vm_id), "payment");
            trx.del_key(&format!("link::vmDistributed::{}", vm_id));
            trx.del_key(&format!("link::VmOwnerProgram::{}", vm_id));
            trx.del_key(&format!(
                "link::VmContainerName::{}::{}::{}",
                program.id, input.entity_id, vm_id
            ));
            trx.del_key(&format!(
                "link::vmStandaloneImageName::{}::{}",
                program.id, input.entity_id
            ));
            trx.del_key(&format!(
                "link::vmStandaloneContainerName::{}::{}",
                program.id, input.entity_id
            ));
            Ok(json!({"ok": true, "vmId": vm_id, "result": result}))
        },
    )
}

fn read_vm_logs(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ReadVmLogsInput, _>(
        app,
        "/machines/readVmLogs",
        user_guard(),
        move |state: Arc<dyn IState>, input: ReadVmLogsInput| -> Result<Value> {
            let trx = state.trx();
            // A workload's logs are its VMM's (A501 `logs`, ADR 0030): the instance
            // link names the workload, and the program it belongs to authorizes the
            // read. Build output is that workload's `build` stream — the node-wide
            // "main" build stream any user could read is gone with the host bridge.
            let suffix = format!("::{}", input.vm_id);
            let instance = trx
                .get_links_list("VmInstance::", -1, -1, &[])
                .unwrap_or_default()
                .into_iter()
                .filter(|link| link.ends_with(&suffix))
                .find_map(|link| {
                    let parts: Vec<&str> = link.split("::").collect();
                    let [_, program, entity, _] = parts[..] else {
                        return None;
                    };
                    Some((program.to_owned(), entity.to_owned()))
                });
            let Some((program_id, entity_id)) = instance else {
                return Err(anyhow!("vm not found"));
            };
            let program = (crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx })
                .program_or_empty(&program_id);
            if program.id.is_empty() {
                return Err(anyhow!("vm not found"));
            }
            if resolve_program_owner_machine(&*trx, &program).owner_id != state.info().user_id() {
                return Err(anyhow!("you are not owner of this vm"));
            }
            let remote = crate::shell::workloads::remote()
                .ok_or_else(|| anyhow!("this node has no VMM (ASEMAN_VMM_ENDPOINT)"))?;
            let count = usize::try_from(input.count).unwrap_or(0).clamp(1, 1000);
            let after = u64::try_from(input.offset).unwrap_or(0);
            let build = input.log_type == "build";
            let logs: Vec<Value> = remote
                .logs(
                    crate::shell::workloads::RemoteWorkloads::workload_id(
                        &program.id,
                        &entity_id,
                        &input.vm_id,
                    ),
                    after,
                )?
                .into_iter()
                .filter(|record| (record.stream == aseman_domain::vmm::LogStream::Build) == build)
                .take(count)
                .map(|record| {
                    // The legacy log shape, with the VMM's sequence as the cursor a
                    // reader pages with.
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
            Ok(json!({"logs": logs}))
        },
    )
}

/// List the VM instances recorded for one program entity and ask its runtime
/// plugin for the current process/container state.
fn list_entity_vms(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<RunProgramEntityInput, _>(
        app,
        "/machines/listEntityVms",
        user_guard(),
        move |state: Arc<dyn IState>, input: RunProgramEntityInput| -> Result<Value> {
            let trx = state.trx();
            let program_id = if input.program_id.is_empty() {
                input.machine_id.clone()
            } else {
                input.program_id.clone()
            };
            if aseman_ports::ProgramDirectory::program(
                &crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx },
                &program_id,
            )
            .map_err(|error| anyhow!("{error}"))?
            .is_none()
            {
                return Err(anyhow!("program does not exist"));
            }
            let program = (crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx })
                .program_or_empty(&program_id.clone());
            let owner_machine = resolve_program_owner_machine(&*trx, &program);
            if owner_machine.owner_id != state.info().user_id() {
                return Err(anyhow!("you are not owner of this program"));
            }
            let entity = read_entity(&app_for_handler, &*trx, &program.id, &input.entity_id)?
                .ok_or_else(|| anyhow!("entity does not exist"))?;

            let entity_type = normalize_entity_type(&entity.entity_type);
            let remote = crate::shell::workloads::remote()
                .ok_or_else(|| anyhow!("this node has no VMM (ASEMAN_VMM_ENDPOINT)"))?;
            let prefix = format!("VmInstance::{}::{}::", program.id, input.entity_id);
            let links = trx.get_links_list(&prefix, -1, -1, &[]).unwrap_or_default();
            let mut instances: Vec<Value> = Vec::new();

            for link in links {
                let vm_id = link.strip_prefix(&prefix).unwrap_or(&link).to_string();
                if vm_id.is_empty() {
                    continue;
                }
                let recorded_status = trx.get_link(&format!("VmStatus::{}", vm_id));
                let started_at = trx
                    .get_link(&format!("VmStartedAt::{}", vm_id))
                    .parse::<i64>()
                    .unwrap_or(0);
                // What the VMM observes for the instance's workload.
                let probe: Result<Value, String> =
                    serde_json::from_str::<Value>(&remote.vm_host_call(
                        &app_for_handler,
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
        },
    )
}

fn open_vm_terminal(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<VmTerminalInput, _>(
        app,
        "/machines/openVmTerminal",
        user_guard(),
        move |state: Arc<dyn IState>, input: VmTerminalInput| -> Result<Value> {
            let trx = state.trx();
            if aseman_ports::ProgramDirectory::program(
                &crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx },
                &input.creature_id,
            )
            .map_err(|error| anyhow!("{error}"))?
            .is_none()
            {
                return Err(anyhow!("program does not exist"));
            }
            let program = (crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx })
                .program_or_empty(&input.creature_id.clone());
            let owner_machine = resolve_program_owner_machine(&*trx, &program);
            if owner_machine.owner_id != state.info().user_id() {
                return Err(anyhow!("you are not owner of this creature"));
            }
            trx.put_link(
                &format!(
                    "VmTerminal::{}::{}::{}",
                    input.creature_id,
                    input.vm_id,
                    state.info().user_id()
                ),
                "true",
            );
            Ok(json!({"terminal": "on"}))
        },
    )
}

fn close_vm_terminal(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<VmTerminalInput, _>(
        app,
        "/machines/closeVmTerminal",
        user_guard(),
        move |state: Arc<dyn IState>, input: VmTerminalInput| -> Result<Value> {
            let trx = state.trx();
            if aseman_ports::ProgramDirectory::program(
                &crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx },
                &input.creature_id,
            )
            .map_err(|error| anyhow!("{error}"))?
            .is_none()
            {
                return Err(anyhow!("program does not exist"));
            }
            let program = (crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx })
                .program_or_empty(&input.creature_id.clone());
            let owner_machine = resolve_program_owner_machine(&*trx, &program);
            if owner_machine.owner_id != state.info().user_id() {
                return Err(anyhow!("you are not owner of this creature"));
            }
            trx.del_key(&format!(
                "link::VmTerminal::{}::{}::{}",
                input.creature_id,
                input.vm_id,
                state.info().user_id()
            ));
            Ok(json!({"terminal": "off"}))
        },
    )
}

fn read_machine_builds(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<MachineBuildsInput, _>(
        app,
        "/machines/readMachineBuilds",
        user_guard(),
        move |state: Arc<dyn IState>, input: MachineBuildsInput| -> Result<Value> {
            let prefix = format!("VmBuilds::{}::", input.machine_id);
            let builds =
                state
                    .trx()
                    .get_links_list(&prefix, input.offset, input.count, &[false])?;
            Ok(json!({"buildsList": builds}))
        },
    )
}

/// Record (or clear) an entity's custom VM gateway route, reconciling any route
/// a previous deploy of the same entity left behind. `creature_id` is the
/// program's owning creature (whose username the route is reached by),
/// `gateway_path` the normalized prefix (empty ⇒ the entity exposes no custom
/// route) and `gateway_vm_id` an optional specific instance to target.
pub(crate) fn register_gateway_route(
    trx: &dyn ITrx,
    creature_id: &str,
    program_id: &str,
    entity_id: &str,
    gateway_path: &str,
    gateway_vm_id: &str,
    runtime: &str,
) -> Result<()> {
    use aseman_ports::GatewayRoutes;
    let routes = crate::shell::api::model::gateway_ports::GatewayPorts { trx };
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
    let username = (crate::shell::api::model::creature_ports::CreaturePorts { trx })
        .creature_or_empty(&creature_id.to_string())
        .username;
    let local_part = crate::drivers::vmm::http_route::username_local_part(&username);
    if !local_part.is_empty() && local_part != creature_id {
        routes.put_alias(local_part, creature_id).map_err(failed)?;
    }
    Ok(())
}

fn deploy(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<DeployInput, _>(
        app,
        "/programs/deploy",
        user_guard(),
        move |state: Arc<dyn IState>, input: DeployInput| -> Result<Value> {
            let trx = state.trx();
            let program_id = input.machine_id.clone();
            if aseman_ports::ProgramDirectory::program(
                &crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx },
                &program_id,
            )
            .map_err(|error| anyhow!("{error}"))?
            .is_none()
            {
                return Err(anyhow!("program not found"));
            }
            let program = (crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx })
                .program_or_empty(&program_id.clone());
            // Authorize against the recorded program owner (no app_id).
            let owner_machine = resolve_program_owner_machine(&*trx, &program);
            if owner_machine.owner_id != state.info().user_id() {
                return Err(anyhow!("access to vm denied"));
            }
            let entity_type = normalize_entity_type(&input.entity_type);
            // Proxy entities are non-runnable: the deploy stores the payload
            // as the entity's data file plus a target descriptor. Signals to
            // the entity are forwarded to the target with the file attached
            // and responses are routed back through the proxy (see
            // drivers::vmm::proxy). No plugin, no build, no billing.
            if entity_type == crate::drivers::vmm::proxy::PROXY_RUNTIME_KEY {
                let config = crate::drivers::vmm::proxy::config_from_metadata(|k| {
                    input.metadata.get(k).cloned()
                })
                .map_err(|e| anyhow!(e))?;
                let data = base64::engine::general_purpose::STANDARD
                    .decode(&input.payload)
                    .map_err(|e| anyhow!("{}", e))?;
                let blobs =
                    crate::drivers::blob_store::node_blobs(&*app_for_handler.tools().storage());
                let evidence =
                    blobs.put_entity_file(&program.id, &input.entity_id, "proxy.data", &data)?;
                crate::drivers::vmm::proxy::record_proxy_entity(
                    &*trx,
                    &blobs,
                    &program.id,
                    &input.entity_id,
                    &evidence,
                    &config,
                )?;
                // Register the signal listener so the proxy entity actually
                // receives (and forwards) signals addressed to this program.
                app_for_handler.tools().workloads().assign(&program.id);
                return Ok(json!({
                    "proxy": true,
                    "entityId": input.entity_id,
                    "entityType": crate::drivers::vmm::proxy::PROXY_RUNTIME_KEY,
                    "target": config.to_value(),
                }));
            }
            // The VMM declares how each runtime's entities deploy: the primary
            // file name, whether extra files are accepted, and whether a build
            // must precede the first start (the VMM builds then).
            let remote = crate::shell::workloads::remote()
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
            // The developer chooses the deployment scope: "cluster" ships the
            // creature to every instance of this origin (edge execution +
            // raft-propagated state), "local" pins it to this instance and
            // keeps all of its VM state out of the consensus.
            let distributed = input.wants_distribution() && crate::drivers::cluster::is_active();
            let distribution_label = if distributed { "cluster" } else { "local" };
            let blobs = crate::drivers::blob_store::node_blobs(&*app_for_handler.tools().storage());
            let primary =
                blobs.put_entity_file(&program.id, &input.entity_id, &primary_file_name, &data)?;
            // Artifact files shipped to the other instances on a distributed
            // deploy (base64 as received; primary file first).
            let mut artifact_files: Vec<(String, String)> =
                vec![(primary_file_name.clone(), input.payload.clone())];
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
                    artifact_files.push((k.clone(), data_str));
                }
            }
            // Custom VM gateway route: the deployer may bind this entity's HTTP
            // server to a friendly `/{creatureUsername}/{gatewayPath…}` path.
            // Stored on chain keyed by the owning creature + normalized prefix
            // so it replicates with the deploy and the ingress can resolve it.
            let gateway_path = crate::drivers::vmm::http_route::normalize_path(
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
                &*trx,
                &program.machine_id,
                &program.id,
                &input.entity_id,
                &gateway_path,
                &gateway_vm_id,
                &entity_type,
            )?;
            // Register the machine signal listener for every runtime. Without
            // this, signals addressed to a creature's program are dropped (no
            // listener) and every creature-to-creature signal silently times
            // out.
            app_for_handler.tools().workloads().assign(&program.id);
            aseman_application::program::RecordEntityDeployment {
                entities: &EntityPorts {
                    trx: &*trx,
                    blobs: &blobs,
                },
            }
            .execute(&aseman_application::program::EntityDeployment {
                entity: EntityRecord {
                    program_id: program.id.clone(),
                    entity_id: input.entity_id.clone(),
                    entity_type: entity_type.clone(),
                    image_name: input.entity_id.clone(),
                },
                primary,
                // The VMM fetches every runtime's primary file (P5-06).
                runtime_file: true,
                // Downloadable entities (front-end scripts executed on the
                // client) are served at any time via /programs/downloadEntity.
                downloadable: input.downloadable,
                config: None,
            })
            .map_err(|error| anyhow!("{error}"))?;
            // Persist the chosen scope; the VMM consults these links to decide
            // whether a VM's state mutations enter the raft consensus.
            trx.put_link(
                &format!("vmDistribution::{}", program.id),
                distribution_label,
            );
            trx.put_link(
                &format!("vmDistribution::{}::{}", program.id, input.entity_id),
                distribution_label,
            );
            if distributed {
                crate::drivers::cluster::propose_deploy(
                    crate::drivers::cluster::command::DeployArtifact {
                        program_id: program.id.clone(),
                        entity_id: input.entity_id.clone(),
                        entity_type: entity_type.clone(),
                        machine_id: program.machine_id.clone(),
                        runtime: program.runtime.clone(),
                        path: program.path.clone(),
                        comment: program.comment.clone(),
                        primary_file_name: primary_file_name.clone(),
                        files: artifact_files,
                        set_entity_links: true,
                        build_on_deploy: conventions.build_on_deploy,
                        gateway_route: gateway_path.clone(),
                        gateway_vm_id: gateway_vm_id.clone(),
                    },
                );
            }
            let mut result = serde_json::to_value(PlugInput::default())?;
            if let Value::Object(map) = &mut result {
                map.insert("distribution".into(), json!(distribution_label));
            }
            Ok(result)
        },
    )
}

/// `/programs/downloadEntity` — hand a deployed downloadable entity's file to
/// the caller (base64). This is how front-end apps deployed as entities are
/// fetched and executed on the client side at any time.
fn download_entity(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<DownloadEntityInput, _>(
        app,
        "/programs/downloadEntity",
        user_guard(),
        move |state: Arc<dyn IState>, input: DownloadEntityInput| -> Result<Value> {
            let trx = state.trx();
            let program_id = if input.program_id.is_empty() {
                input.machine_id.clone()
            } else {
                input.program_id.clone()
            };
            if program_id.is_empty() || input.entity_id.is_empty() {
                return Err(anyhow!("programId and entityId are required"));
            }
            let blobs = crate::drivers::blob_store::node_blobs(&*app_for_handler.tools().storage());
            let entities = EntityPorts {
                trx: &*trx,
                blobs: &blobs,
            };
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
        },
    )
}

fn list_machines(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ListInput, _>(
        app,
        "/machines/list",
        user_guard(),
        move |state: Arc<dyn IState>, input: ListInput| -> Result<Value> {
            let trx = state.trx();
            // "Machines" are just creatures of type "machine".
            let creatures = crate::shell::api::model::creature_ports::CreaturePorts { trx: &*trx };
            let machines = aseman_application::creature::GetCreature {
                directory: &creatures,
                balances: &creatures,
            }
            .list(Some("machine"), input.offset, Some(input.count))
            .map_err(crate::shell::api::model::store_ports::legacy_error)?
            .into_iter()
            .map(|found| {
                crate::shell::api::model::creature_ports::creature_view(found.record, found.balance)
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
    build_secure_action::<ListInput, _>(
        app,
        "/programs/list",
        user_guard(),
        move |state: Arc<dyn IState>, input: ListInput| -> Result<Value> {
            let trx = state.trx();
            let count = (input.count != -1).then_some(input.count);
            let machines = aseman_ports::ProgramDirectory::programs(
                &crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx },
                input.offset,
                count,
            )
            .map_err(|error| anyhow!("{error}"))?
            .into_iter()
            .map(crate::shell::api::model::program_ports::program_view)
            .collect::<Vec<_>>();
            Ok(json!({"machines": machines}))
        },
    )
}

fn list_program_machines(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ListAppMachsInput, _>(
        app,
        "/machines/listProgramMachines",
        user_guard(),
        move |state: Arc<dyn IState>, input: ListAppMachsInput| -> Result<Value> {
            let trx = state.trx();
            let programs = aseman_ports::ProgramDirectory::programs_of_machine(
                &crate::shell::api::model::program_ports::ProgramPorts { trx: &*trx },
                &input.app_id,
            )
            .map_err(|error| anyhow!("{error}"))?
            .into_iter()
            .map(crate::shell::api::model::program_ports::program_view)
            .collect::<Vec<_>>();
            // Legacy lists the creatures whose identity equals a linked program id.
            let users = programs
                .iter()
                .filter_map(|program| {
                    aseman_ports::CreatureDirectory::creature(
                        &crate::shell::api::model::creature_ports::CreaturePorts { trx: &*trx },
                        &program.id,
                    )
                    .ok()
                    .flatten()
                })
                .map(|record| crate::shell::api::model::creature_ports::creature_view(record, 0))
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
        },
    )
}

/// Mirror of Go's `Install`: walk the existing programs, hand each one to the
/// VMM and replay any pending vm-alarm. The 15-second billing ticker is
/// intentionally not started here — see the module doc-comment.
fn install_program_bootstrap(app: Arc<dyn ICore>) {
    let app_for_closure = app.clone();
    app.modify_state(
        true,
        Box::new(move |trx: &dyn ITrx| {
            let programs = aseman_ports::ProgramDirectory::programs(
                &crate::shell::api::model::program_ports::ProgramPorts { trx },
                0,
                None,
            )
            .map_err(|error| anyhow!("{error}"))?
            .into_iter()
            .map(crate::shell::api::model::program_ports::program_view)
            .collect::<Vec<_>>();
            for program in programs {
                let is_proxy = normalize_entity_type(&program.runtime)
                    == crate::drivers::vmm::proxy::PROXY_RUNTIME_KEY;
                let is_vm = crate::shell::workloads::remote()
                    .is_some_and(|remote| remote.offers(&program.runtime));
                // Proxy programs are non-runnable, but their signal listener must
                // still be re-registered on restart so forwarded prompts reach
                // them; only real VM runtimes additionally replay a pending alarm.
                if is_proxy || is_vm {
                    app_for_closure.tools().workloads().assign(&program.id);
                }
                if is_vm {
                    let pending = aseman_ports::ProgramAlarms::alarm(
                        &crate::shell::api::model::program_ports::ProgramPorts { trx },
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
                let ports = crate::shell::api::model::store_ports::MembershipPorts { trx };
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
    crate::drivers::vmm::proxy::start_correlation_reaper(app.clone());
    let billing_lock = Arc::new(Mutex::new(-1i64));
    let app_bg = app.clone();
    std::thread::spawn(move || loop {
        charge_running_standalone_vms_if_needed(&app_bg, &billing_lock);
        std::thread::sleep(std::time::Duration::from_secs(15));
    });
}
