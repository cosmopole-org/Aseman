//! Translation of `drivers/vmm/hostcall_logs.go`.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::models::transaction::ITrx;
use crate::shell::api::model::{Creature, Program};

use super::driver::{check_str, now_unix_ms, Vmm};

impl Vmm {
    pub(super) fn resolve_vm_ownership(&self, vm_id: &str) -> (String, String) {
        if vm_id.is_empty() {
            return (String::new(), String::new());
        }
        let creature_slot = Arc::new(Mutex::new(String::new()));
        let owner_slot = Arc::new(Mutex::new(String::new()));
        let creature_clone = creature_slot.clone();
        let owner_clone = owner_slot.clone();
        let suffix = format!("::{}", vm_id);
        self.app.modify_state(
            true,
            Box::new(move |trx: &dyn ITrx| {
                let links = match trx.get_links_list("VmInstance::", -1, -1, &[]) {
                    Ok(l) => l,
                    Err(_) => return Ok(()),
                };
                for link in links {
                    if !link.ends_with(&suffix) {
                        continue;
                    }
                    let parts: Vec<&str> = link.split("::").collect();
                    if parts.len() < 4 {
                        continue;
                    }
                    let creature_id = parts[1].to_string();
                    *creature_clone.lock().unwrap() = creature_id.clone();
                    let program = Program {
                        id: creature_id.clone(),
                        ..Default::default()
                    }
                    .pull(trx);
                    if program.id.is_empty() {
                        break;
                    }
                    let machine =
                        (crate::shell::api::model::creature_ports::LegacyCreatures { trx })
                            .creature_or_empty(&program.machine_id.clone());
                    *owner_clone.lock().unwrap() = machine.owner_id;
                    break;
                }
                Ok(())
            }),
        );
        let creature_id = creature_slot.lock().unwrap().clone();
        let owner_id = owner_slot.lock().unwrap().clone();
        (creature_id, owner_id)
    }

    pub(super) fn handle_vm_log_event(&self, input: &Value, req_id: i64) -> (String, i64) {
        let mut data = check_str(input, "text", "");
        if data.is_empty() {
            data = check_str(input, "data", "");
        }
        if data.is_empty() {
            return ("{}".into(), req_id);
        }
        let vm_id = check_str(input, "vmId", "");
        let mut log_type = check_str(input, "logType", "");
        if log_type.is_empty() {
            log_type = "runtime".into();
        }
        let time_val = now_unix_ms();
        if vm_id.is_empty() {
            return ("{}".into(), req_id);
        }
        let (creature_id, owner_user_id) = self.resolve_vm_ownership(&vm_id);
        let packet = self.storage.log_vm(&vm_id, &log_type, &data, time_val);
        if !owner_user_id.is_empty() {
            let key = format!("VmTerminal::{}::{}::{}", creature_id, vm_id, owner_user_id);
            let terminal_slot = Arc::new(Mutex::new(false));
            let term_clone = terminal_slot.clone();
            let key_owned = key.clone();
            self.app.modify_state(
                true,
                Box::new(move |trx: &dyn ITrx| {
                    *term_clone.lock().unwrap() = trx.get_link(&key_owned) == "true";
                    Ok(())
                }),
            );
            if *terminal_slot.lock().unwrap() {
                let log_payload = serde_json::to_value(&packet).unwrap_or(Value::Null);
                self.app.tools().signaler().signal_user(
                    "machines/vmLogs",
                    &owner_user_id,
                    json!({"log": log_payload}),
                    true,
                );
            }
        }
        if data.trim().is_empty() {
            return ("{}".into(), req_id);
        }
        ("{}".into(), req_id)
    }
}
