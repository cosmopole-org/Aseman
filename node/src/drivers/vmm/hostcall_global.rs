//! Translation of `drivers/vmm/hostcall_global.go`.

use serde_json::Value;

use super::driver::{check_i64, check_str, Vmm};

impl Vmm {
    /// Entry point: parse a host-call message coming from the appengine over
    /// ZMQ and route it to the right handler. Returns `(result_json,
    /// requestId)` so the caller can produce an `apiResponse` envelope.
    pub fn vm_callback(&self, data_raw: &str) -> (String, i64) {
        let data: Value = match serde_json::from_str(data_raw) {
            Ok(v) => v,
            Err(e) => return (format!("{{\"error\":\"{}\"}}", e), 0),
        };
        let req_id = check_i64(&data, "requestId", 0);
        let key = check_str(&data, "key", "");
        let input = data.get("input").cloned().unwrap_or(Value::Null);

        match key.as_str() {
            // Multi-runtime VM lifecycle ops are handled by the typed VM packet
            // router (route_vm_packet dispatches by "type"), the same path
            // build_vm_image uses. The host-call envelope carries the fields
            // under "input" with the op in "key"; translate to a typed packet
            // (fields hoisted to top level, key->type) and dispatch it so docker
            // and firecracker creatures actually start/exec/copy.
            "runVm" | "execVm" | "execDocker" | "statusVm" | "copyToVm" | "copyToDocker"
            | "copyFromVm" => {
                let typed = match key.as_str() {
                    "execDocker" => "execVm",
                    "copyToDocker" => "copyToVm",
                    other => other,
                };
                let mut packet = input.clone();
                if let Some(obj) = packet.as_object_mut() {
                    obj.insert("type".into(), Value::String(typed.to_string()));
                    let res = crate::drivers::vmm::dispatch_packet(&packet);
                    (res, req_id)
                } else {
                    (
                        "{\"ok\":false,\"error\":\"vm op input must be an object\"}".into(),
                        req_id,
                    )
                }
            }
            "checkTokenValidity" => self.handle_check_token_validity(&input, req_id),
            "plantTrigger" => self.handle_plant_trigger(&input, req_id),
            "signal" => self.handle_signal_store(&input, req_id),
            "terminateVm" => self.handle_terminate_vm(&input, req_id),
            "sendMessageOnChain" => self.handle_send_message_on_chain(&input, req_id),
            "createProgram" => self.handle_program_crud("create", &input, req_id),
            "deleteProgram" | "deleteOwnedProgram" => {
                self.handle_program_crud("delete", &input, req_id)
            }
            "getProgram" => self.handle_program_crud("get", &input, req_id),
            "listPrograms" => self.handle_program_crud("list", &input, req_id),
            "listProgramMachines" => self.handle_program_crud("listByMachine", &input, req_id),
            "updateProgram" => self.handle_program_crud("update", &input, req_id),
            "deployEntity" => self.handle_deploy_entity(&input, req_id),
            "createCreature" => self.handle_creature_crud("create", &input, req_id),
            "updateCreature" => self.handle_creature_crud("update", &input, req_id),
            "deleteCreature" => self.handle_creature_crud("delete", &input, req_id),
            "getCreature" => self.handle_creature_crud("get", &input, req_id),
            "listCreatures" => self.handle_creature_crud("list", &input, req_id),
            "createResourceStore" | "createVmOwnedStore" => {
                self.handle_resource_store_crud("create", &input, req_id)
            }
            "updateResourceStore" | "updateVmOwnedStore" => {
                self.handle_resource_store_crud("update", &input, req_id)
            }
            "deleteResourceStore" | "deleteVmOwnedStore" => {
                self.handle_resource_store_crud("delete", &input, req_id)
            }
            "getResourceStore" | "getVmOwnedStore" => {
                self.handle_resource_store_crud("get", &input, req_id)
            }
            "listResourceStores" | "listVmOwnedStores" => {
                self.handle_resource_store_crud("list", &input, req_id)
            }
            "createStore" => self.handle_store_crud("create", &input, req_id),
            "updateStore" => self.handle_store_crud("update", &input, req_id),
            "deleteStore" => self.handle_store_crud("delete", &input, req_id),
            "getStore" => self.handle_store_crud("get", &input, req_id),
            "listStores" => self.handle_store_crud("list", &input, req_id),
            "createResourceEntity" => self.handle_resource_entity_create(&input, req_id),
            "deleteResourceEntity" => self.handle_resource_entity_delete(&input, req_id),
            "createWorkchain" | "deleteWorkchain" | "createSubchain" | "deleteSubchain" => {
                self.handle_vm_chain_request(&key, &input, req_id)
            }
            // The appengine callback carries no verified VM identity, so a call
            // arriving here can only act as an identity it names — `asSelf` is
            // refused. The unified host-call surface resolves the caller first
            // (see `vm_host_functions`), which is the path a container takes.
            "execShellAction" => self.handle_exec_shell_action("", &input, req_id),
            "genId" | "getLink" | "delKey" | "createAccess" | "updateAccess" | "deleteAccess"
            | "getJson" | "putJson" | "getByPrefix" | "hasAccessToStore" | "readSignals"
            | "signalUser" | "signalGroup" | "joinGroup" => {
                self.handle_micro_host_action(&key, &input, req_id)
            }
            "log" | "vmLog" | "buildLog" | "output" | "vmOutput" => {
                self.handle_vm_log_event(&input, req_id)
            }
            _ => ("{}".into(), req_id),
        }
    }
}
