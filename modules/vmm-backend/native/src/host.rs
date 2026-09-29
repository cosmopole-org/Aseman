//! The runtime plugins' `VmHost` in the backend process.
//!
//! Everything a plugin asks of its host is served here from backend-local state, or
//! forwarded to the node's guest API as the workload the plugin runs, signed with that
//! workload's credential (P5-04). The workload is identified by the VM or machine the
//! runtime stamped on the packet, which the backend itself assigned when it started
//! the instance; nothing a guest writes selects it. There is no other path to the
//! node: no storage handle, no shell action, no global application.
//!
//! Buffered-write hooks (`begin_vm_buffer`, `commit_vm_buffer`, `end_vm_json_trx`)
//! are no-ops: every guest write is its own request, as on the node's PostgreSQL
//! provider (ADR 0021).

use std::sync::Arc;

use aseman_contracts::guest_api::WorkloadCredential;
use aseman_domain::vmm::LogStream;
use aseman_guest_http::client::GuestApiClient;
use caspar_vm_sdk::KvOp;
use caspar_vm_sdk::host::VmHost;
use serde_json::{Value, json};

use crate::registry::{PluginState, Registry};

/// The guest API call that serves a runtime's creature-scoped key/value state
/// (`{machine}::{key}` in the legacy runtime key space); the node confines it to the
/// calling workload's creature.
pub const STATE_OP: &str = "stateOp";

pub struct NativeHost {
    pub registry: Registry,
    pub state: Arc<PluginState>,
    pub guest: Arc<GuestApiClient>,
    pub storage_root: String,
}

pub(crate) fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

fn refused(reason: &str) -> String {
    json!({"ok": false, "error": reason}).to_string()
}

fn stream_of(log_type: &str) -> LogStream {
    match log_type {
        "error" | "stderr" => LogStream::Stderr,
        "build" | "buildLog" => LogStream::Build,
        "runtime" | "system" => LogStream::System,
        _ => LogStream::Stdout,
    }
}

impl NativeHost {
    /// The credential of the workload a packet names by VM, then by machine.
    fn credential_for(&self, vm_id: &str, machine_id: &str) -> Option<Arc<WorkloadCredential>> {
        self.registry
            .with_vm(vm_id, |instance| instance.credential.clone())
            .or_else(|| {
                self.registry
                    .by_runtime_key(&format!("{machine_id}::"))
                    .map(|(credential, _, _)| credential)
            })
    }

    fn log(&self, vm_id: &str, log_type: &str, text: &str, at_millis: i64) -> bool {
        self.registry
            .with_vm(vm_id, |instance| {
                instance.log(stream_of(log_type), text, at_millis);
            })
            .is_some()
    }

    /// Call the node as a workload; an answer the node refused becomes the legacy
    /// `{ok: false, error}` shape runtimes expect.
    fn call(&self, credential: &WorkloadCredential, op: &str, input: &Value) -> String {
        match self
            .guest
            .call(credential, op, input.to_string().as_bytes())
        {
            Ok(bytes) => String::from_utf8(bytes).unwrap_or_else(|_| refused("non-UTF-8 answer")),
            Err(error) => refused(&error.to_string()),
        }
    }

    fn state_call(&self, credential: &WorkloadCredential, input: &Value) -> Result<Value, String> {
        let answer = self.call(credential, STATE_OP, input);
        let value: Value = serde_json::from_str(&answer).map_err(|error| error.to_string())?;
        if value["ok"] == false {
            return Err(value["error"].as_str().unwrap_or("refused").to_owned());
        }
        Ok(value)
    }
}

impl VmHost for NativeHost {
    fn dispatch(&self, packet: &Value) -> String {
        let key = packet["key"].as_str().unwrap_or("");
        let input = &packet["input"];
        let vm_id = input["vmId"].as_str().unwrap_or("");
        match key {
            "log" | "vmLog" | "buildLog" | "output" | "vmOutput" => {
                let text = input["text"]
                    .as_str()
                    .or_else(|| input["data"].as_str())
                    .unwrap_or("");
                let log_type = input["logType"].as_str().unwrap_or(if key == "buildLog" {
                    "build"
                } else {
                    "stdout"
                });
                if self.log(vm_id, log_type, text, now_millis()) {
                    json!({"ok": true}).to_string()
                } else {
                    refused("unknown vm")
                }
            }
            // A runtime's own signal, trigger, or termination, made as its workload.
            "signal" | "plantTrigger" | "terminateVm" => {
                let machine_id = input["machineId"].as_str().unwrap_or("");
                match self.credential_for(vm_id, machine_id) {
                    Some(credential) => self.call(&credential, key, input),
                    None => refused("this operation needs an identified caller"),
                }
            }
            _ if packet["type"] == "hostCall" => self.unified_host_call(packet),
            // Lifecycle packets need an identified caller: a guest reaches other VMs
            // only through its node (the `runVm`/`terminateVm` host calls).
            _ => refused("this operation needs an identified caller"),
        }
    }

    fn unified_host_call(&self, packet: &Value) -> String {
        let op = packet["op"]
            .as_str()
            .or_else(|| packet["key"].as_str())
            .unwrap_or("");
        let vm_id = packet["vmId"].as_str().unwrap_or("");
        let machine_id = packet["machineId"].as_str().unwrap_or("");
        let input = &packet["input"];
        if matches!(op, "vmLog" | "consoleLog") {
            let text = input["text"]
                .as_str()
                .map_or_else(|| input.to_string(), str::to_owned);
            let log_type = input["logType"].as_str().unwrap_or("stdout");
            return if self.log(vm_id, log_type, &text, now_millis()) {
                json!({"ok": true}).to_string()
            } else {
                refused("unknown vm")
            };
        }
        match self.credential_for(vm_id, machine_id) {
            Some(credential) => self.call(&credential, op, input),
            None => refused("this operation needs an identified caller"),
        }
    }

    fn storage_log_vm(&self, vm_id: &str, log_type: &str, text: &str, timestamp_ms: i64) {
        self.log(vm_id, log_type, text, timestamp_ms);
    }

    fn register_vm_context(&self, vm_id: &str, _creature_id: &str, machine_id: &str) {
        self.registry.alias_vm(vm_id, machine_id);
    }

    fn unregister_vm_context(&self, vm_id: &str) {
        self.registry.unalias_vm(vm_id);
    }

    fn get_vm_context(&self, vm_id: &str) -> Option<(String, String)> {
        self.registry.with_vm(vm_id, |instance| {
            (instance.machine_id.clone(), instance.machine_id.clone())
        })
    }

    fn register_vm_container(
        &self,
        container_name: &str,
        _vm_id: &str,
        _creature_id: &str,
        _program_id: &str,
        machine_id: &str,
        _entity_id: &str,
    ) {
        self.registry.alias_vm(container_name, machine_id);
    }

    fn unregister_vm_container(&self, container_name: &str) {
        self.registry.unalias_vm(container_name);
    }

    fn begin_vm_buffer(&self, _vm_id: &str) {}

    fn commit_vm_buffer(&self, _vm_id: &str) {}

    fn acquire_resource_lock(&self, resource_id: &str, owner_id: &str) -> Result<(), String> {
        self.lock_call("lockResource", resource_id, owner_id)
    }

    fn release_resource_lock(&self, resource_id: &str, owner_id: &str) -> Result<(), String> {
        self.lock_call("unlockResource", resource_id, owner_id)
    }

    fn state_get(&self, key: &str) -> String {
        match self.registry.by_runtime_key(key) {
            Some((credential, _, rest)) => self
                .state_call(&credential, &json!({"op": "get", "key": rest}))
                .ok()
                .and_then(|value| value["data"].as_str().map(str::to_owned))
                .unwrap_or_default(),
            None => self.state.get(key),
        }
    }

    fn state_get_by_prefix(&self, prefix: &str) -> Vec<String> {
        match self.registry.by_runtime_key(prefix) {
            Some((credential, _, rest)) => self
                .state_call(&credential, &json!({"op": "getByPrefix", "prefix": rest}))
                .ok()
                .and_then(|value| {
                    value["data"].as_array().map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str().map(str::to_owned))
                            .collect()
                    })
                })
                .unwrap_or_default(),
            None => self.state.by_prefix(prefix),
        }
    }

    fn state_apply_ops(&self, ops: &[KvOp]) -> Result<(), String> {
        let mut local = Vec::new();
        for op in ops {
            let delete = match op.op.as_str() {
                "put" => false,
                "del" => true,
                other => return Err(format!("unsupported state op {other}")),
            };
            match self.registry.by_runtime_key(&op.key) {
                Some((credential, _, rest)) => {
                    let input = if delete {
                        json!({"op": "del", "key": rest})
                    } else {
                        json!({"op": "put", "key": rest, "val": op.val})
                    };
                    self.state_call(&credential, &input)?;
                }
                None => local.push((op.key.clone(), (!delete).then(|| op.val.clone()))),
            }
        }
        if local.is_empty() {
            Ok(())
        } else {
            self.state.apply(&local)
        }
    }

    fn vm_json_trx_op(&self, vm_id: &str, op: &str, input: &Value) -> Result<Value, String> {
        let base = caspar_vm_sdk::util::trx_key_vm_id(vm_id);
        let credential = self
            .credential_for(base, "")
            .ok_or_else(|| "this operation needs an identified caller".to_owned())?;
        let answer = self.call(&credential, op, input);
        serde_json::from_str(&answer).map_err(|error| error.to_string())
    }

    fn end_vm_json_trx(&self, _vm_id: &str) {}

    fn http_request(&self, input: &Value) -> Result<String, String> {
        let vm_id = input["vmId"].as_str().unwrap_or("");
        let credential = self
            .credential_for(vm_id, input["machineId"].as_str().unwrap_or(""))
            .ok_or_else(|| "this operation needs an identified caller".to_owned())?;
        self.guest
            .call(&credential, "httpRequest", input.to_string().as_bytes())
            .map_err(|error| error.to_string())
            .and_then(|bytes| String::from_utf8(bytes).map_err(|error| error.to_string()))
    }

    fn storage_root(&self) -> String {
        self.storage_root.clone()
    }
}

impl NativeHost {
    fn lock_call(&self, op: &str, resource_id: &str, owner_id: &str) -> Result<(), String> {
        let credential = self
            .registry
            .with_vm(owner_id, |instance| instance.credential.clone())
            .ok_or_else(|| "this operation needs an identified caller".to_owned())?;
        let answer: Value = serde_json::from_str(&self.call(
            &credential,
            op,
            &json!({"resourceId": resource_id, "ownerId": owner_id}),
        ))
        .map_err(|error| error.to_string())?;
        if answer["ok"] == true {
            Ok(())
        } else {
            Err(answer["error"].as_str().unwrap_or("refused").to_owned())
        }
    }
}
