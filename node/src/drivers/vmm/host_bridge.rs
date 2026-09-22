//! The node-side implementation of the `caspar_vm_sdk::VmHost` interface.
//!
//! This is the single point where VM plugins reach back into the Caspar node.
//! Every capability is served through the canonical
//! `ICore → tools() → vmm()` object graph (via the VMM's global app handle),
//! so plugins never see node internals and the compiler checks the whole
//! contract through the SDK traits.

use std::sync::{Arc, Once};

use serde_json::{json, Value as JsonValue};

use caspar_vm_sdk::host::VmHost;
use caspar_vm_sdk::KvOp;

use crate::drivers::vmm::globals::with_global_app;
use crate::models::transaction::ITrx;

/// Register the built-in VM plugins (compiled in by the generated
/// `caspar-vm-plugins` crate) and publish the host bridge. Idempotent.
pub fn init_vm_plugins() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        caspar_vm_sdk::set_host(Arc::new(VmmHostBridge));
        caspar_vm_plugins::register_all();
    });
}

/// A runtime `dbOp` key (`{creature}::{guestKey}`) served by the creature's guest
/// database when the node runs on PostgreSQL (ADR 0021); `None` otherwise.
fn runtime_guest_op(op: &str, key: &str, value: &str) -> Option<Result<String, String>> {
    let (creature, guest_key) = crate::shell::api::model::guest_data::split_runtime_key(key)?;
    crate::shell::api::model::guest_data::route_db_op(
        creature,
        aseman_domain::guest::LegacyKvNamespace::DbOp,
        op,
        guest_key,
        value,
        guest_key,
    )
}

/// `VmHost` served by the Caspar VMM.
pub struct VmmHostBridge;

impl VmHost for VmmHostBridge {
    fn dispatch(&self, packet: &JsonValue) -> String {
        crate::drivers::vmm::dispatch_packet(packet)
    }

    fn unified_host_call(&self, packet: &JsonValue) -> String {
        crate::drivers::vmm::host::vm_host_functions::handle_unified_host_call(packet)
    }

    fn storage_log_vm(&self, vm_id: &str, log_type: &str, text: &str, timestamp_ms: i64) {
        let _ = with_global_app(|app| {
            let _ = app
                .tools()
                .storage()
                .log_vm(vm_id, log_type, text, timestamp_ms);
        });
    }

    fn register_vm_context(&self, vm_id: &str, creature_id: &str, machine_id: &str) {
        let _ = with_global_app(|app| {
            app.tools()
                .vmm()
                .register_vm_context(vm_id, creature_id, machine_id)
        });
    }

    fn unregister_vm_context(&self, vm_id: &str) {
        let _ = with_global_app(|app| app.tools().vmm().unregister_vm_context(vm_id));
    }

    fn get_vm_context(&self, vm_id: &str) -> Option<(String, String)> {
        with_global_app(|app| app.tools().vmm().get_vm_context(vm_id)).flatten()
    }

    fn register_vm_container(
        &self,
        container_name: &str,
        vm_id: &str,
        creature_id: &str,
        program_id: &str,
        machine_id: &str,
        entity_id: &str,
    ) {
        let _ = with_global_app(|app| {
            app.tools().vmm().register_vm_container(
                container_name,
                vm_id,
                creature_id,
                program_id,
                machine_id,
                entity_id,
            )
        });
    }

    fn unregister_vm_container(&self, container_name: &str) {
        let _ = with_global_app(|app| app.tools().vmm().unregister_vm_container(container_name));
    }

    fn begin_vm_buffer(&self, vm_id: &str) {
        let _ = with_global_app(|app| app.tools().vmm().begin_vm_trx(vm_id));
    }

    fn commit_vm_buffer(&self, vm_id: &str) {
        let _ = with_global_app(|app| app.tools().vmm().commit_vm_trx(vm_id));
    }

    fn acquire_resource_lock(&self, resource_id: &str, owner_id: &str) -> Result<(), String> {
        match with_global_app(|app| {
            app.tools()
                .vmm()
                .acquire_resource_lock(resource_id, owner_id)
        }) {
            Some(result) => result,
            None => Err("vmm not initialised".to_string()),
        }
    }

    fn release_resource_lock(&self, resource_id: &str, owner_id: &str) -> Result<(), String> {
        match with_global_app(|app| {
            app.tools()
                .vmm()
                .release_resource_lock(resource_id, owner_id)
        }) {
            Some(result) => result,
            None => Err("vmm not initialised".to_string()),
        }
    }

    fn state_get(&self, key: &str) -> String {
        if let Some(value) = runtime_guest_op("get", key, "") {
            return value
                .ok()
                .and_then(|text| serde_json::from_str::<JsonValue>(&text).ok())
                .and_then(|value| value["data"].as_str().map(str::to_owned))
                .unwrap_or_default();
        }
        let key = key.to_string();
        with_global_app(move |app| {
            let slot = Arc::new(std::sync::Mutex::new(String::new()));
            let slot_c = slot.clone();
            app.modify_state(
                true,
                Box::new(move |trx: &dyn ITrx| {
                    *slot_c.lock().unwrap() = trx.get_link(&key);
                    Ok(())
                }),
            );
            let v = { slot.lock().unwrap().clone() };
            v
        })
        .unwrap_or_default()
    }

    fn state_get_by_prefix(&self, prefix: &str) -> Vec<String> {
        if let Some(values) = runtime_guest_op("getByPrefix", prefix, "") {
            return values
                .ok()
                .and_then(|text| serde_json::from_str::<JsonValue>(&text).ok())
                .and_then(|value| {
                    value["data"].as_array().map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str().map(str::to_owned))
                            .collect()
                    })
                })
                .unwrap_or_default();
        }
        let prefix = prefix.to_string();
        with_global_app(move |app| {
            let slot = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let slot_c = slot.clone();
            app.modify_state(
                true,
                Box::new(move |trx: &dyn ITrx| {
                    *slot_c.lock().unwrap() = trx.get_by_prefix(&prefix);
                    Ok(())
                }),
            );
            let v = { slot.lock().unwrap().clone() };
            v
        })
        .unwrap_or_default()
    }

    fn state_apply_ops(&self, ops: &[KvOp]) -> Result<(), String> {
        if ops.is_empty() {
            return Ok(());
        }
        // Runtime `dbOp` pairs go to their creature's guest database on PostgreSQL;
        // plugin state keeps its legacy home.
        let mut legacy = Vec::with_capacity(ops.len());
        for op in ops {
            match runtime_guest_op(&op.op, &op.key, &op.val) {
                Some(result) => {
                    result?;
                }
                None => legacy.push(op.clone()),
            }
        }
        if legacy.is_empty() {
            return Ok(());
        }
        let ops = legacy;
        match with_global_app(move |app| {
            app.modify_state(
                false,
                Box::new(move |trx: &dyn ITrx| {
                    for op in &ops {
                        if op.op == "put" {
                            trx.put_link(&op.key, &op.val);
                        } else if op.op == "del" {
                            // A put is stored as a link (`link::<key>`). Deleting the
                            // bare key removed nothing, so plugin state could never be
                            // cleared — the Modal provisioning marker outlived every
                            // successful start and made a running machine read as
                            // "provisioning", then "failed".
                            trx.del_key(&format!("link::{}", op.key));
                        }
                    }
                    Ok(())
                }),
            );
        }) {
            Some(()) => Ok(()),
            None => Err("core not initialised".to_string()),
        }
    }

    fn vm_json_trx_op(
        &self,
        vm_id: &str,
        op: &str,
        input: &JsonValue,
    ) -> Result<JsonValue, String> {
        // The creature is the one the node registered for this VM when it started
        // it; `vm_id` is the runtime's own transaction key, never guest input.
        let base_vm_id = caspar_vm_sdk::util::trx_key_vm_id(vm_id);
        let creature = with_global_app(|app| app.tools().vmm().get_vm_context(base_vm_id))
            .flatten()
            .map(|(creature, _)| creature)
            .unwrap_or_default();
        let trx = with_global_app(|app| app.begin_vm_trx(vm_id))
            .ok_or_else(|| "ICore not initialised".to_string())?;
        crate::drivers::vmm::guest_state::run(&*trx, &creature, op, input)
    }

    fn end_vm_json_trx(&self, vm_id: &str) {
        let _ = with_global_app(|app| app.end_vm_trx(vm_id));
    }

    fn http_request(&self, input: &JsonValue) -> Result<String, String> {
        crate::drivers::vmm::host::vm_host_functions::perform_http_request(input)
    }

    fn storage_root(&self) -> String {
        with_global_app(|app| app.tools().storage().storage_root().to_string()).unwrap_or_default()
    }
}
