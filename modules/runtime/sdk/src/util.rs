//! Shared helpers used by the VMM and by every VM plugin.

use base64::Engine;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Canonical runtime-name normalisation (`strings.ToLower(TrimSpace(.))`).
pub fn normalize_runtime(runtime: &str) -> String {
    runtime.trim().to_lowercase()
}

/// Resource limits attached to a VM execution request.
#[derive(Clone, Debug)]
pub struct VmResourceLimits {
    pub max_exec_time_secs: u64,
    pub ram_mb: u64,
    pub disk_gb: u64,
    pub cpu_cores: u64,
}

impl VmResourceLimits {
    pub fn with_defaults() -> Self {
        VmResourceLimits {
            max_exec_time_secs: 60,
            ram_mb: 64,
            disk_gb: 1,
            cpu_cores: 1,
        }
    }
}

/// Parse the `resources` object of a VM packet, falling back to defaults.
pub fn parse_vm_resource_limits(packet: &Value) -> VmResourceLimits {
    let mut limits = VmResourceLimits::with_defaults();
    let resources = &packet["resources"];
    if resources.is_object() {
        limits.max_exec_time_secs = resources["maxExecTimeSeconds"]
            .as_u64()
            .unwrap_or(60)
            .max(1);
        limits.ram_mb = resources["ramMb"].as_u64().unwrap_or(64).max(1);
        limits.disk_gb = resources["diskGb"].as_u64().unwrap_or(1).max(1);
        limits.cpu_cores = resources["cpuCores"].as_u64().unwrap_or(1).max(1);
    }
    limits
}

/// Parse a JSON array field of u64s.
pub fn parse_u64_array_field(packet: &Value, field_name: &str) -> Vec<u64> {
    packet[field_name]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
        .unwrap_or_default()
}

/// Parse a byte-blob field: either a base64 string (compact — preferred for
/// large blobs like STARK proofs) or a raw JSON number array.
pub fn parse_u8_array_field(packet: &Value, field_name: &str) -> Vec<u8> {
    match &packet[field_name] {
        Value::String(s) => base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .unwrap_or_default(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
            .collect(),
        _ => Vec::new(),
    }
}

/// The host JSON-transaction key for ONE execution of a VM.
///
/// A signal-driven run carries no `vmId`, so every concurrent execution of every
/// program resolves to `"main"` — and the host keys its per-VM JSON transaction
/// by that id. Sharing it let one run's teardown commit and retire a transaction
/// other runs were still writing into, and their later writes landed in a
/// finalized transaction and were silently dropped (a suspended continuation
/// vanished, so the answer to it resumed nothing). Each execution therefore gets
/// its own key. The part before `#` is still the VM id, which is what the host
/// uses for anything identity-shaped — see [`trx_key_vm_id`].
pub fn execution_trx_key(vm_id: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}#exec-{:x}-{:x}", vm_id, nanos, seq)
}

/// The VM id an [`execution_trx_key`] belongs to (a plain VM id maps to itself).
pub fn trx_key_vm_id(key: &str) -> &str {
    key.split('#').next().unwrap_or(key)
}

/// Extract a human-readable message from a `catch_unwind` payload.
pub fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic payload>".to_string()
}

/// Publish a uniform error packet for any VM runtime failure so the host
/// observer sees the failure instead of a silent drop.
pub fn emit_vm_error(machine_id: &str, vm_id: &str, runtime: &str, err: &str) {
    if let Some(h) = crate::host::host() {
        let _ = h.dispatch(&json!({
            "key": "vmOutput",
            "input": {
                "text": err,
                "data": err,
                "vmId": vm_id,
                "machineId": machine_id,
                "logType": "error",
                "runtime": runtime,
            }
        }));
    }
}
