//! Node startup services that are not operations (ADR 0040): the built-in
//! creature types, and what workloads need running on a started node. These stay
//! node-side composition; the operation plugins never see them.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use aseman_action_sdk::state::creature_ports::{creature_type, put_creature_type};
use aseman_action_sdk::state::program_ports::{ProgramPorts, program_view};
use aseman_action_sdk::state::store_ports::MembershipPorts;
use aseman_action_sdk::state::vm_runtime;
use aseman_action_sdk::util::async_once;
use aseman_ports::{ProgramAlarms, ProgramDirectory, StoreAccess};
use serde_json::{Value, json};

use crate::node::Node;
use crate::storage::Trx;
use crate::workloads::proxy;
use crate::workloads::vmm::{RemoteWorkloads, creature_subject, program_machine};

const DEFAULT_CREATURE_INITIAL_BALANCE: i64 = 0;
const LEGACY_HUMAN_INITIAL_BALANCE: i64 = 1_000_000_000_000_000;

/// Register the built-in creature types (idempotent), and replace the old
/// built-in human grant without overwriting a host-defined balance.
///
/// # Errors
///
/// Storage failures.
pub(crate) fn install_creature_types(node: &Node) -> Result<()> {
    node.in_action(|trx| {
        for (name, desc) in [
            ("human", "The primary human being on the network."),
            ("machine", "A non-human being that can own programs."),
        ] {
            if creature_type(trx, name)?.is_none() {
                put_creature_type(
                    trx,
                    name,
                    &json!({
                        "initialBalance": DEFAULT_CREATURE_INITIAL_BALANCE,
                        "customFields": [],
                        "desc": desc,
                    }),
                )?;
            }
        }
        if let Some(mut spec) = creature_type(trx, "human")?
            && spec.get("initialBalance").and_then(Value::as_i64)
                == Some(LEGACY_HUMAN_INITIAL_BALANCE)
        {
            spec.insert(
                "initialBalance".to_owned(),
                json!(DEFAULT_CREATURE_INITIAL_BALANCE),
            );
            put_creature_type(trx, "human", &Value::Object(spec))?;
        }
        Ok(())
    })
}

/// Start what workloads need running on a started node: restore every program's
/// signal delivery and pending alarm, reap proxy correlations whose answer never
/// came, and charge running standalone instances every minute.
///
/// # Errors
///
/// Storage failures while reading the programs.
pub(crate) fn start_workload_services(node: &Arc<Node>) -> Result<()> {
    restore_programs(node)?;
    proxy::start_correlation_reaper(node.clone());
    let node = node.clone();
    std::thread::spawn(move || {
        let mut last_minute = -1_i64;
        loop {
            charge_running_instances(&node, &mut last_minute);
            std::thread::sleep(Duration::from_secs(15));
        }
    });
    Ok(())
}

/// Hand every program to the VMM listener registry again (their listeners are
/// in-memory), rejoin their stores, and replay each pending alarm.
fn restore_programs(node: &Arc<Node>) -> Result<()> {
    struct Restore {
        program_id: String,
        runnable: bool,
        alarm: Option<aseman_domain::program::ProgramAlarm>,
        stores: Vec<String>,
    }
    let restores = node.in_action(|trx| {
        let programs = ProgramPorts { trx };
        let mut restores = Vec::new();
        for record in programs
            .programs(0, None)
            .map_err(|error| anyhow!("{error}"))?
        {
            let program = program_view(record);
            let is_proxy = program.runtime.trim().to_lowercase() == proxy::PROXY_RUNTIME_KEY;
            let is_vm = node
                .vmm()
                .is_some_and(|remote| remote.offers(&program.runtime));
            let alarm = if is_vm {
                programs
                    .alarm(&program.id)
                    .map_err(|error| anyhow!("{error}"))?
            } else {
                None
            };
            restores.push(Restore {
                stores: MembershipPorts { trx }
                    .stores_of(&program.id)
                    .unwrap_or_default(),
                program_id: program.id,
                // A proxy program is not runnable, but its signal listener must be
                // registered so forwarded prompts reach it.
                runnable: is_proxy || is_vm,
                alarm,
            });
        }
        Ok(restores)
    })?;
    let workloads = node.tools().workloads();
    let signaler = node.tools().signaler();
    for restore in restores {
        if restore.runnable {
            workloads.assign(&restore.program_id);
        }
        for store in &restore.stores {
            signaler.join_group(store, &restore.program_id);
        }
        if let Some(alarm) = restore.alarm {
            let node = node.clone();
            let program_id = restore.program_id.clone();
            async_once(move || {
                let wait = alarm.fire_at_millis - chrono::Utc::now().timestamp_millis();
                if wait > 0 {
                    std::thread::sleep(Duration::from_millis(u64::try_from(wait).unwrap_or(0)));
                }
                if node
                    .tools()
                    .security()
                    .has_access_to_store(&program_id, &alarm.store_id)
                {
                    node.tools().workloads().run_vm_entity(
                        &program_id,
                        &alarm.store_id,
                        &alarm.data,
                        &alarm.entity,
                    );
                }
            });
        }
    }
    Ok(())
}

/// One charge of a running paid instance, or its stop when its steps are spent.
struct Charge {
    vm_id: String,
    machine_id: String,
    entity_id: String,
    /// `None` when the instance has no signed step left.
    step: Option<(String, String, i64, i64, String)>,
}

/// Charge each running paid instance one step through its payment lock, once a
/// minute; stop the instances whose steps are spent or whose charge fails.
fn charge_running_instances(node: &Arc<Node>, last_minute: &mut i64) {
    let current_minute = chrono::Utc::now().timestamp() / 60;
    if *last_minute == current_minute {
        return;
    }
    let charges = match node.in_action(|trx| due_charges(trx, current_minute)) {
        Ok(charges) => charges,
        Err(error) => {
            eprintln!("vm billing: {error}");
            return;
        }
    };
    for mut charge in charges {
        let Some((payer, lock_id, step, amount, signature)) = charge.step.take() else {
            stop_instance(node, &charge);
            continue;
        };
        let payload = json!({
            "type": "pay",
            "userId": payer,
            "lockId": lock_id,
            "signature": signature,
            "amount": amount,
            "step": step,
        });
        if consume_on_chain(node, &payload) {
            let vm_id = charge.vm_id.clone();
            let recorded = node.in_action(|trx| {
                let mut billing = vm_runtime::instance(trx, &vm_id)?
                    .and_then(|instance| instance.billing)
                    .and_then(|billing| billing.as_object().cloned())
                    .unwrap_or_default();
                let step = billing.get("currentStep").and_then(as_i64).unwrap_or(0);
                billing.insert("currentStep".into(), json!(step + 1));
                billing.insert("lastChargeMinute".into(), json!(current_minute));
                vm_runtime::set_billing(trx, &vm_id, billing)
            });
            if let Err(error) = recorded {
                eprintln!("vm billing: cannot record the charge of {vm_id}: {error}");
            }
        } else {
            stop_instance(node, &charge);
        }
    }
    *last_minute = current_minute;
}

fn as_i64(raw: &Value) -> Option<i64> {
    match raw {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

fn due_charges(trx: &Trx, current_minute: i64) -> Result<Vec<Charge>> {
    let mut charges = Vec::new();
    for instance in vm_runtime::billed_running(trx)? {
        let Some(billing) = instance
            .billing
            .and_then(|billing| billing.as_object().cloned())
        else {
            continue;
        };
        let text = |key: &str| {
            billing
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let Some(next_step) = billing.get("currentStep").and_then(as_i64) else {
            continue;
        };
        let per_minute_cost = billing.get("perMinuteCost").and_then(as_i64).unwrap_or(0);
        let last_charge = billing
            .get("lastChargeMinute")
            .and_then(as_i64)
            .unwrap_or(0);
        let signatures: Vec<String> = billing
            .get("signatures")
            .and_then(Value::as_array)
            .map(|all| {
                all.iter()
                    .map(|s| s.as_str().unwrap_or("").to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let (payer, lock_id, machine_id, entity_id) = (
            text("payerUserId"),
            text("lockId"),
            text("machineId"),
            text("entityId"),
        );
        if payer.is_empty()
            || lock_id.is_empty()
            || per_minute_cost <= 0
            || machine_id.is_empty()
            || entity_id.is_empty()
            || last_charge == current_minute
            || next_step < 0
        {
            continue;
        }
        let step = match usize::try_from(next_step)
            .ok()
            .and_then(|i| signatures.get(i))
        {
            Some(signature) => Some((
                payer,
                lock_id,
                next_step,
                per_minute_cost,
                signature.clone(),
            )),
            None if last_charge < current_minute => None,
            None => continue,
        };
        charges.push(Charge {
            vm_id: instance.key,
            machine_id,
            entity_id,
            step,
        });
    }
    Ok(charges)
}

/// Consume a lock step through `/creatures/consumeLock` on the chain, as the node
/// owner; whether it was consumed within 30 seconds.
fn consume_on_chain(node: &Arc<Node>, payload: &Value) -> bool {
    let bytes = serde_json::to_vec(payload).unwrap_or_default();
    let signature = node.sign_packet_as_owner(&bytes);
    let (sender, receiver) = std::sync::mpsc::channel::<bool>();
    node.globe().send_base_request_on_chain(
        "/creatures/consumeLock",
        bytes,
        &signature,
        &node.owner_id(),
        "",
        Box::new(move |_data, status, error| {
            let _ = sender.send(error.is_none() && status < 400);
        }),
    );
    receiver
        .recv_timeout(Duration::from_secs(30))
        .unwrap_or(false)
}

/// Stop a billed instance as the owner of its program, whose ownership the node
/// established when it launched.
fn stop_instance(node: &Arc<Node>, charge: &Charge) {
    let Some(remote) = node.vmm() else {
        eprintln!("cannot stop {}: this node has no VMM", charge.vm_id);
        return;
    };
    let owner = program_machine(node, &charge.machine_id);
    if let Err(error) = remote.set_state_as(
        creature_subject(&owner),
        RemoteWorkloads::workload_id(&charge.machine_id, &charge.entity_id, &charge.vm_id),
        aseman_domain::DesiredWorkloadState::Stopped,
    ) {
        eprintln!("cannot stop {}: {error}", charge.vm_id);
    }
    let vm_id = charge.vm_id.clone();
    if let Err(error) = node.in_action(|trx| vm_runtime::mark_stopped(trx, &vm_id)) {
        eprintln!("cannot record the stop of {}: {error}", charge.vm_id);
    }
}