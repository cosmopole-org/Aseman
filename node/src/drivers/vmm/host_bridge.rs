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
        let ops: Vec<KvOp> = ops.to_vec();
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
        let trx = with_global_app(|app| app.begin_vm_trx(vm_id))
            .ok_or_else(|| "ICore not initialised".to_string())?;
        match op {
            "putJson" => {
                let key = input["key"].as_str().unwrap_or("");
                let path = input["path"].as_str().unwrap_or("");
                let merge = input["merge"].as_bool().unwrap_or(true);
                let obj = input["data"].clone();
                let _ = trx.put_json(key, path, &obj, merge);
                Ok(json!({"ok": true}))
            }
            "getJson" => {
                let key = input["key"].as_str().unwrap_or("");
                let path = input["path"].as_str().unwrap_or("");
                match trx.get_json(key, path) {
                    Ok(m) => Ok(json!({"ok": true, "data": JsonValue::Object(m)})),
                    Err(_) => Ok(json!({"ok": true, "data": {}})),
                }
            }
            "getByPrefix" => {
                // putJson stores keys as "json::key::path"; search within that
                // namespace and strip the prefix so callers see the same key
                // space they wrote to.
                let prefix = input["prefix"].as_str().unwrap_or("");
                let json_prefix = format!("json::{}", prefix);
                let keys = trx.get_by_prefix(&json_prefix);
                let stripped: Vec<String> = keys
                    .into_iter()
                    .map(|k| k.strip_prefix("json::").unwrap_or(&k).to_string())
                    .collect();
                Ok(json!({"ok": true, "data": stripped}))
            }
            "delKey" => {
                let key = input["key"].as_str().unwrap_or("");
                let path = input["path"].as_str().unwrap_or("");
                // A document written with putJson lives at `json::<key>::<path>`.
                // Deleting only the raw key tombstoned a key nothing reads and left
                // the document in place, so "deleted" continuations, questions and
                // index rows all survived their deletion.
                if path.is_empty() {
                    trx.del_key(key);
                } else {
                    trx.del_json(key, path);
                }
                Ok(json!({"ok": true}))
            }
            _ => Err(format!("unsupported vm json trx op: {}", op)),
        }
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
