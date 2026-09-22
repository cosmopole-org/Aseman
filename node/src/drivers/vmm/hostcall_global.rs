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
            // This callback protocol carries no verified caller identity, so it
            // serves only what trusted runtime and node code sends: output and log
            // events, a runtime's own trigger and signal (its machine stamped by
            // the runtime), and the node's own terminations. Everything a guest may
            // ask for goes through the unified host call, which identifies and
            // authorizes the caller (LD-14).
            "terminateVm" => self.handle_terminate_vm(&input, req_id),
            "plantTrigger" => self.handle_plant_trigger(&input, req_id),
            "signal" => self.handle_signal_store(&input, req_id),
            "log" | "vmLog" | "buildLog" | "output" | "vmOutput" => {
                self.handle_vm_log_event(&input, req_id)
            }
            _ => (
                r#"{"ok":false,"error":"this operation needs an identified caller"}"#.into(),
                req_id,
            ),
        }
    }
}
