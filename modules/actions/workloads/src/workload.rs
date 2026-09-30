//! The entity and workload handlers.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use aseman_action_sdk::state::creature_ports::CreaturePorts;
use aseman_action_sdk::state::entity_ports::EntityPorts;
use aseman_action_sdk::state::gateway_ports::GatewayPorts;
use aseman_action_sdk::state::program_ports::{ProgramPorts, owner_machine, program_view};
use aseman_action_sdk::state::{token_locks, vm_runtime};
use aseman_action_sdk::state::Program;
use aseman_action_sdk::util::{Ctx, LaunchResources, Trx};
use aseman_action_sdk::wire::program::{
    DeployInput, DownloadEntityInput, MachineBuildsInput, ReadVmLogsInput, RunProgramEntityInput,
    VmResourcesInput, VmTerminalInput,
};
use aseman_action_sdk::context::ActionVmm;
use aseman_action_sdk::proxy;
use aseman_contracts::vm_routes as http_route;
use aseman_domain::program::{ArtifactRole, EntityRecord};
use aseman_ports::{BlobStore, EntityDirectory, ProgramDirectory};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

fn normalize_entity_type(value: &str) -> String {
    value.trim().to_lowercase()
}

fn as_i64(raw: &Value) -> Option<i64> {
    match raw {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

fn vmm(ctx: &Ctx<'_>) -> Result<Arc<dyn ActionVmm>> {
    ctx.node
        .vmm()
        .ok_or_else(|| anyhow!("this node has no VMM (ASEMAN_VMM_ENDPOINT)"))
}

/// The program `program_id` names, owned by the caller's machine.
fn owned_program(ctx: &Ctx<'_>, program_id: &str, refusal: &str) -> Result<Program> {
    let programs = ProgramPorts { trx: ctx.trx };
    let Some(record) = programs
        .program(program_id)
        .map_err(|error| anyhow!("{error}"))?
    else {
        return Err(anyhow!("program does not exist"));
    };
    let program = program_view(record);
    if owner_machine(ctx.trx, &program).owner_id != ctx.caller.user_id {
        return Err(anyhow!("{refusal}"));
    }
    Ok(program)
}

fn entity(trx: &Trx, program_id: &str, entity_id: &str) -> Result<EntityRecord> {
    EntityPorts { trx }
        .entity(program_id, entity_id)
        .map_err(|error| anyhow!("{error}"))?
        .ok_or_else(|| anyhow!("entity does not exist"))
}

/// Record (or clear) an entity's custom gateway route, dropping a route an earlier
/// deploy of the same entity left behind. `creature_id` is the program's machine,
/// whose username the route is reached by.
fn register_gateway_route(
    trx: &Trx,
    creature_id: &str,
    program_id: &str,
    entity_id: &str,
    gateway_path: &str,
    gateway_vm_id: &str,
    runtime: &str,
) -> Result<()> {
    use aseman_ports::GatewayRoutes;
    let routes = GatewayPorts { trx };
    let failed = |error: aseman_ports::PortError| anyhow!("{error}");
    if let Some((previous_creature, previous_path)) = routes
        .route_of_entity(program_id, entity_id)
        .map_err(failed)?
    {
        let unchanged = !gateway_path.is_empty()
            && previous_creature == creature_id
            && previous_path == gateway_path;
        if !unchanged {
            routes
                .delete_route(&previous_creature, &previous_path)
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
    // The bare local part of the machine's username addresses the route too.
    let username = CreaturePorts { trx }
        .creature_or_empty(creature_id)
        .username;
    let local_part = http_route::username_local_part(&username);
    if !local_part.is_empty() && local_part != creature_id {
        routes.put_alias(local_part, creature_id).map_err(failed)?;
    }
    Ok(())
}

// ── Deploy and download ───────────────────────────────────────────────────────

/// Deploy an entity of a program: store its files, record it, bind its gateway
/// route, and register the program's signal delivery. A `proxy` entity forwards
/// signals to another creature instead of running.
pub fn deploy(ctx: &Ctx<'_>, input: DeployInput) -> Result<Value> {
    let program = owned_program(ctx, &input.machine_id, "access to vm denied")?;
    let entity_type = normalize_entity_type(&input.entity_type);
    let decode = |text: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(|error| anyhow!("{error}"))
    };
    let blobs = ctx.node.tools().storage().blob_store();
    let workloads = ctx.node.tools().workloads();
    if entity_type == proxy::PROXY_RUNTIME_KEY {
        let config = proxy::config_from_metadata(|key| input.metadata.get(key).cloned())
            .map_err(|error| anyhow!(error))?;
        let evidence = blobs.put_entity_file(
            &program.id,
            &input.entity_id,
            "proxy.data",
            &decode(&input.payload)?,
        )?;
        proxy::record_proxy_entity(ctx.trx, &program.id, &input.entity_id, &evidence, &config)?;
        workloads.assign(&program.id);
        return Ok(json!({
            "proxy": true,
            "entityId": input.entity_id,
            "entityType": proxy::PROXY_RUNTIME_KEY,
            "target": config.to_value(),
        }));
    }
    let remote = vmm(ctx)?;
    let conventions = remote.deploy_conventions(&entity_type).ok_or_else(|| {
        anyhow!(
            "invalid entityType, expected one of {}",
            remote.runtime_keys().join("|")
        )
    })?;
    let primary = blobs.put_entity_file(
        &program.id,
        &input.entity_id,
        &conventions.entity_file_name,
        &decode(&input.payload)?,
    )?;
    if conventions.accepts_extra_files {
        match input.metadata.get("files") {
            None | Some(Value::Null) => {}
            Some(Value::Object(files)) => {
                for (name, content) in files {
                    let Value::String(content) = content else {
                        return Err(anyhow!("file bytecode not string"));
                    };
                    blobs.put_entity_file(
                        &program.id,
                        &input.entity_id,
                        name,
                        &decode(content)?,
                    )?;
                }
            }
            Some(_) => return Err(anyhow!("files is not map")),
        }
    }
    let text = |key: &str| {
        input
            .metadata
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    register_gateway_route(
        ctx.trx,
        &program.machine_id,
        &program.id,
        &input.entity_id,
        &http_route::normalize_path(&text("gatewayPath")),
        &text("gatewayVmId"),
        &entity_type,
    )?;
    workloads.assign(&program.id);
    aseman_application::program::RecordEntityDeployment {
        entities: &EntityPorts { trx: ctx.trx },
    }
    .execute(&aseman_application::program::EntityDeployment {
        entity: EntityRecord {
            program_id: program.id.clone(),
            entity_id: input.entity_id.clone(),
            entity_type,
            image_name: input.entity_id.clone(),
        },
        primary,
        runtime_file: true,
        downloadable: input.downloadable,
        config: None,
    })
    .map_err(|error| anyhow!("{error}"))?;
    let distribution = if input.wants_distribution() {
        "cluster"
    } else {
        "local"
    };
    vm_runtime::set_distribution(ctx.trx, &program.id, None, distribution)?;
    vm_runtime::set_distribution(ctx.trx, &program.id, Some(&input.entity_id), distribution)?;
    Ok(json!({"distribution": distribution}))
}

/// A deployed downloadable entity's file (base64), for a client to run itself
/// (a deployed front-end app, for one).
pub fn download_entity(ctx: &Ctx<'_>, input: DownloadEntityInput) -> Result<Value> {
    let program_id = if input.program_id.is_empty() {
        input.machine_id
    } else {
        input.program_id
    };
    if program_id.is_empty() || input.entity_id.is_empty() {
        return Err(anyhow!("programId and entityId are required"));
    }
    let entities = EntityPorts { trx: ctx.trx };
    let artifact = entities
        .artifact(&program_id, &input.entity_id, ArtifactRole::Downloadable)
        .map_err(|error| anyhow!("{error}"))?
        .ok_or_else(|| anyhow!("entity is not downloadable"))?;
    let entity = entities
        .entity(&program_id, &input.entity_id)
        .map_err(|error| anyhow!("{error}"))?
        .unwrap_or_default();
    let blobs = ctx.node.tools().storage().blob_store();
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

// ── Instances ─────────────────────────────────────────────────────────────────

/// What a launched standalone instance may use: non-positive fields take the
/// defaults.
#[derive(Clone, Debug, Serialize, Deserialize)]
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

impl VmResources {
    fn from_input(input: &VmResourcesInput) -> Self {
        let defaults = LaunchResources::default();
        let or = |value: i64, default: i64| if value <= 0 { default } else { value };
        Self {
            max_exec_time_seconds: or(input.max_exec_time_seconds, defaults.max_exec_time_seconds),
            ram_mb: or(input.ram_mb, defaults.ram_mb),
            disk_gb: or(input.disk_gb, defaults.disk_gb),
            cpu_cores: or(input.cpu_cores, defaults.cpu_cores),
        }
    }

    /// The instance's price per minute at the node's rates (at least 1).
    fn per_minute_cost(&self, ctx: &Ctx<'_>) -> i64 {
        let costs = ctx.node.vm_costs();
        let cost = self.ram_mb * costs.ram_per_mb_minute
            + self.cpu_cores * costs.cpu_core_per_minute
            + self.disk_gb * costs.disk_per_gb_minute;
        cost.max(1)
    }
}

/// The billing record of a paid standalone launch, after checking its payment
/// lock: payable to this node's owner, one step per minute at the instance's
/// price, each step signed by the payer.
fn launch_billing(
    ctx: &Ctx<'_>,
    lock_id: &str,
    signatures: &[String],
    resources: &VmResources,
) -> Result<Map<String, Value>> {
    let payer = &ctx.caller.user_id;
    if lock_id.is_empty() {
        return Err(anyhow!(
            "paymentLockId is required for standalone vm execution"
        ));
    }
    let payment = token_locks::lock(ctx.trx, payer, lock_id)?
        .ok_or_else(|| anyhow!("payment lock not found"))?;
    let owner = ctx.node.owner_id();
    if payment.get("userId").and_then(Value::as_str) != Some(owner.as_str()) {
        return Err(anyhow!("payment lock target is invalid"));
    }
    let steps = match payment.get("steps") {
        Some(Value::Array(steps)) if !steps.is_empty() => steps.clone(),
        _ => return Err(anyhow!("payment lock does not include steps")),
    };
    if signatures.len() != steps.len() {
        return Err(anyhow!(
            "paymentSignatures count must match lock steps count"
        ));
    }
    let per_minute_cost = resources.per_minute_cost(ctx);
    let mut previous_unlock = None;
    for (i, step) in steps.iter().enumerate() {
        let Value::Object(step) = step else {
            return Err(anyhow!("invalid payment lock step"));
        };
        let amount = step.get("amount").and_then(as_i64).unwrap_or(0);
        if amount != per_minute_cost {
            return Err(anyhow!(
                "payment lock step amount must match vm per-minute resource cost"
            ));
        }
        let unlock_at = step.get("unlockAt").and_then(as_i64).unwrap_or(0);
        if unlock_at <= 0 {
            return Err(anyhow!("payment lock step unlockAt is invalid"));
        }
        if previous_unlock.is_some_and(|previous| unlock_at - previous != 60_000) {
            return Err(anyhow!("payment lock steps must be one-minute apart"));
        }
        previous_unlock = Some(unlock_at);
        let signed = format!("{lock_id}:{i}:{unlock_at}:{amount}:{owner}");
        let (verified, _, _) = ctx.node.tools().security().auth_with_signature(
            payer,
            signed.as_bytes(),
            &signatures[i],
        );
        if !verified {
            return Err(anyhow!("payment signature verification failed"));
        }
    }
    let mut billing = Map::new();
    billing.insert("payerUserId".into(), json!(payer));
    billing.insert("lockId".into(), json!(lock_id));
    billing.insert("perMinuteCost".into(), json!(per_minute_cost));
    billing.insert("currentStep".into(), json!(0));
    billing.insert("stepCount".into(), json!(steps.len()));
    billing.insert("lastChargeMinute".into(), json!(-1_i64));
    billing.insert("signatures".into(), json!(signatures));
    billing.insert("resources".into(), serde_json::to_value(resources)?);
    Ok(billing)
}

fn program_id_of(input: &RunProgramEntityInput) -> String {
    if input.program_id.is_empty() {
        input.machine_id.clone()
    } else {
        input.program_id.clone()
    }
}

/// Launch a new instance of an entity on the node's VMM.
pub fn run_entity(ctx: &Ctx<'_>, input: RunProgramEntityInput) -> Result<Value> {
    let program = owned_program(
        ctx,
        &program_id_of(&input),
        "you are not owner of this program",
    )?;
    let entity = entity(ctx.trx, &program.id, &input.entity_id)?;
    let entity_type = normalize_entity_type(&entity.entity_type);
    let vm_id = Uuid::new_v4().to_string();
    let gateway_path = http_route::normalize_path(&input.gateway_path);
    if !gateway_path.is_empty() {
        register_gateway_route(
            ctx.trx,
            &program.machine_id,
            &program.id,
            &input.entity_id,
            &gateway_path,
            &vm_id,
            &entity_type,
        )?;
    }
    let distributed = vm_runtime::distribution(ctx.trx, &program.id, None)? == "cluster"
        || vm_runtime::distribution(ctx.trx, &program.id, Some(&input.entity_id))? == "cluster";
    let resources = VmResources::from_input(&input.resources);
    let billing = if ctx.node.vm_costs().is_free() {
        None
    } else {
        let mut billing = launch_billing(
            ctx,
            &input.payment_lock_id,
            &input.payment_signatures,
            &resources,
        )?;
        billing.insert("machineId".into(), json!(input.machine_id));
        billing.insert("entityId".into(), json!(input.entity_id));
        billing.insert("vmId".into(), json!(vm_id));
        Some(billing)
    };
    let remote = vmm(ctx)?;
    if !remote.offers(&entity_type) {
        return Err(anyhow!("invalid entity type"));
    }
    vm_runtime::record_launch(
        ctx.trx,
        &vm_runtime::Launch {
            vm_id: &vm_id,
            program_id: &program.id,
            entity_id: &input.entity_id,
            started_at_millis: chrono::Utc::now().timestamp_millis(),
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
        LaunchResources {
            cpu_cores: resources.cpu_cores,
            ram_mb: resources.ram_mb,
            disk_gb: resources.disk_gb,
            max_exec_time_seconds: resources.max_exec_time_seconds,
        },
        input.params.into_iter().collect(),
    )?;
    Ok(json!({"vmId": vm_id}))
}

/// Stop (suspend) one instance: it can be resumed, and its volume survives.
pub fn stop_entity(ctx: &Ctx<'_>, input: RunProgramEntityInput) -> Result<Value> {
    let program = owned_program(
        ctx,
        &program_id_of(&input),
        "you are not owner of this program",
    )?;
    entity(ctx.trx, &program.id, &input.entity_id)?;
    vm_runtime::mark_stopped(ctx.trx, &input.vm_id)?;
    let remote = vmm(ctx)?;
    remote.set_state(
        &ctx.caller.user_id,
        remote.workload_id(&program.id, &input.entity_id, &input.vm_id),
        aseman_domain::DesiredWorkloadState::Stopped,
    )?;
    Ok(json!({}))
}

/// Destroy one instance and everything it owns; it cannot come back.
pub fn delete_entity(ctx: &Ctx<'_>, input: RunProgramEntityInput) -> Result<Value> {
    let program = owned_program(
        ctx,
        &program_id_of(&input),
        "you are not owner of this program",
    )?;
    entity(ctx.trx, &program.id, &input.entity_id)?;
    let vm_id = input.vm_id.trim().to_owned();
    if vm_id.is_empty() {
        return Err(anyhow!("vmId is required"));
    }
    let belongs = vm_runtime::instance(ctx.trx, &vm_id)?.is_some_and(|instance| {
        instance.program_ref.as_deref() == Some(program.id.as_str())
            && instance.entity_ref.as_deref() == Some(input.entity_id.as_str())
    });
    if !belongs {
        return Err(anyhow!("vm does not belong to this entity"));
    }
    let remote = vmm(ctx)?;
    let workload = remote.workload_id(&program.id, &input.entity_id, &vm_id);
    let generation = remote.set_state(
        &ctx.caller.user_id,
        workload,
        aseman_domain::DesiredWorkloadState::Deleted,
    )?;
    vm_runtime::forget(ctx.trx, &vm_id)?;
    Ok(json!({"ok": true, "vmId": vm_id, "result": {"ok": true, "generation": generation}}))
}

/// The instances recorded for one entity, newest first, each with the state its
/// runtime reports now.
pub fn list_entity_vms(ctx: &Ctx<'_>, input: RunProgramEntityInput) -> Result<Value> {
    let program = owned_program(
        ctx,
        &program_id_of(&input),
        "you are not owner of this program",
    )?;
    let entity = entity(ctx.trx, &program.id, &input.entity_id)?;
    let entity_type = normalize_entity_type(&entity.entity_type);
    let remote = vmm(ctx)?;
    let mut instances = Vec::new();
    for instance in vm_runtime::instances_of(ctx.trx, &program.id, Some(&input.entity_id))? {
        let recorded = instance.status.clone().unwrap_or_default();
        let probe = serde_json::from_str::<Value>(&remote.vm_host_call(
            "statusVm",
            &program.id,
            &json!({
                "runtime": entity_type,
                "machineId": program.id,
                "entityId": input.entity_id,
                "vmId": instance.key,
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
                let status = value["status"].as_str().unwrap_or("unknown").to_owned();
                let running = value["running"].as_bool().unwrap_or(status == "running");
                (status, running, value)
            }
            Err(error) => (
                if recorded.is_empty() {
                    "stopped"
                } else {
                    "unknown"
                }
                .to_owned(),
                false,
                json!({"error": error}),
            ),
        };
        instances.push(json!({
            "vmId": instance.key,
            "status": status,
            "running": running,
            "recordedStatus": recorded,
            "startedAt": instance.started_at_millis.unwrap_or(0),
            "detail": detail,
        }));
    }
    instances
        .sort_by_key(|instance| std::cmp::Reverse(instance["startedAt"].as_i64().unwrap_or(0)));
    Ok(json!({
        "programId": program.id,
        "entityId": input.entity_id,
        "runtime": entity_type,
        "instances": instances,
    }))
}

/// An instance's log lines (`logType` `build` for its build log), and the typed
/// workload they belong to.
pub fn read_logs(ctx: &Ctx<'_>, input: ReadVmLogsInput) -> Result<Value> {
    let Some((program_id, entity_id)) = vm_runtime::instance(ctx.trx, &input.vm_id)?
        .and_then(|instance| Some((instance.program_ref?, instance.entity_ref?)))
    else {
        return Err(anyhow!("vm not found"));
    };
    let program = ProgramPorts { trx: ctx.trx }.program_or_empty(&program_id);
    if program.id.is_empty() {
        return Err(anyhow!("vm not found"));
    }
    if owner_machine(ctx.trx, &program).owner_id != ctx.caller.user_id {
        return Err(anyhow!("you are not owner of this vm"));
    }
    let count = usize::try_from(input.count).unwrap_or(0).clamp(1, 1000);
    let after = u64::try_from(input.offset).unwrap_or(0);
    let build = input.log_type == "build";
    let remote = vmm(ctx)?;
    let workload_id = remote.workload_id(&program.id, &entity_id, &input.vm_id);
    let logs: Vec<Value> = remote
        .logs(workload_id, after)?
        .into_iter()
        .filter(|record| (record.stream == aseman_domain::vmm::LogStream::Build) == build)
        .take(count)
        .map(|record| {
            json!({
                "id": record.sequence.to_string(),
                "buildId": input.vm_id,
                "creatureId": program.machine_id,
                "vmId": input.vm_id,
                "logType": input.log_type,
                "time": record.at_millis,
                "data": record.line,
            })
        })
        .collect();
    Ok(json!({"logs": logs, "workloadId": workload_id.to_string()}))
}

fn set_terminal(ctx: &Ctx<'_>, input: &VmTerminalInput, open: bool) -> Result<()> {
    owned_program(
        ctx,
        &input.creature_id,
        "you are not owner of this creature",
    )?;
    vm_runtime::set_terminal(
        ctx.trx,
        &input.creature_id,
        &input.vm_id,
        &ctx.caller.user_id,
        open,
    )
}

pub fn open_terminal(ctx: &Ctx<'_>, input: VmTerminalInput) -> Result<Value> {
    set_terminal(ctx, &input, true)?;
    Ok(json!({"terminal": "on"}))
}

pub fn close_terminal(ctx: &Ctx<'_>, input: VmTerminalInput) -> Result<Value> {
    set_terminal(ctx, &input, false)?;
    Ok(json!({"terminal": "off"}))
}

/// Build lists: the VMM owns builds (ADR 0022), so no node keeps one.
pub fn read_builds(_: &Ctx<'_>, _: MachineBuildsInput) -> Result<Value> {
    Ok(json!({"buildsList": Vec::<String>::new()}))
}