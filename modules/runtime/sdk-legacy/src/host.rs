//! The host-side API surface a VM plugin may call.
//!
//! The Caspar node implements [`VmHost`] once (backed by its canonical
//! `ICore → tools() → vmm()` object graph) and publishes it through
//! [`set_host`] during VMM initialisation. Plugins never see node internals —
//! everything they need flows through this trait, which lets the compiler
//! verify every interaction between a VM project and the VMM.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};

/// A buffered key-value operation flushed atomically through the node's
/// consensus-aware state layer.
#[derive(Debug, Clone)]
pub struct KvOp {
    /// `"put"` or `"del"`.
    pub op: String,
    pub key: String,
    pub val: String,
}

/// Host services exposed by the Caspar VMM to VM plugins.
pub trait VmHost: Send + Sync {
    // ── Packet dispatch ───────────────────────────────────────────────────
    /// Route a VM packet through the VMM's unified packet router. This is the
    /// same channel VM runtimes historically used (`wasm_send`) to emit
    /// `vmOutput` / `vmLog` / `signal` packets and to invoke other runtimes
    /// (`runVm`, `terminateVm`, `execVm`, …).
    fn dispatch(&self, packet: &Value) -> String;

    /// Route a host-call operation (`signalUser`, `dbOp`, CRUD ops, …)
    /// through the unified host-call dispatcher.
    fn unified_host_call(&self, packet: &Value) -> String;

    // ── Logging ───────────────────────────────────────────────────────────
    /// Persist one VM log line directly through the node's storage driver
    /// (safe to call from inside async runtimes / capture threads).
    fn storage_log_vm(&self, vm_id: &str, log_type: &str, text: &str, timestamp_ms: i64);

    // ── VM execution context registry ─────────────────────────────────────
    fn register_vm_context(&self, vm_id: &str, creature_id: &str, machine_id: &str);
    fn unregister_vm_context(&self, vm_id: &str);
    fn get_vm_context(&self, vm_id: &str) -> Option<(String, String)>;

    // ── Container identity registry (gateway identification) ─────────────
    fn register_vm_container(
        &self,
        container_name: &str,
        vm_id: &str,
        creature_id: &str,
        program_id: &str,
        machine_id: &str,
        entity_id: &str,
    );
    fn unregister_vm_container(&self, container_name: &str);

    // ── Per-VM lifecycle write-ahead buffer (dbOp transactions) ───────────
    fn begin_vm_buffer(&self, vm_id: &str);
    fn commit_vm_buffer(&self, vm_id: &str);

    // ── Resource locks ────────────────────────────────────────────────────
    fn acquire_resource_lock(&self, resource_id: &str, owner_id: &str) -> Result<(), String>;
    fn release_resource_lock(&self, resource_id: &str, owner_id: &str) -> Result<(), String>;

    // ── Node state (link) database ────────────────────────────────────────
    /// Read a link value from the node state.
    fn state_get(&self, key: &str) -> String;
    /// Read every link value under `prefix`.
    fn state_get_by_prefix(&self, prefix: &str) -> Vec<String>;
    /// Apply a batch of put/del link operations atomically.
    fn state_apply_ops(&self, ops: &[KvOp]) -> Result<(), String>;

    // ── Per-VM JSON transaction (putJson/getJson/getByPrefix/delKey) ──────
    /// Execute one JSON-transaction op for `vm_id`, lazily opening the VM's
    /// lifecycle transaction on first use.
    /// `op` ∈ {"putJson","getJson","getByPrefix","delKey"}.
    fn vm_json_trx_op(&self, vm_id: &str, op: &str, input: &Value) -> Result<Value, String>;
    /// Commit and close the VM's lifecycle JSON transaction (no-op when the
    /// VM never opened one).
    fn end_vm_json_trx(&self, vm_id: &str);

    // ── Misc ──────────────────────────────────────────────────────────────
    /// Perform an HTTP request on behalf of a VM. Returns the base64-encoded
    /// response body.
    fn http_request(&self, input: &Value) -> Result<String, String>;
    /// Root of the node's data storage folder.
    fn storage_root(&self) -> String;
}

static HOST: OnceLock<Arc<dyn VmHost>> = OnceLock::new();

/// Publish the host implementation. Called once by the Caspar node during
/// VMM initialisation; later calls are ignored.
pub fn set_host(host: Arc<dyn VmHost>) {
    let _ = HOST.set(host);
}

/// The published host, if the node has initialised the VMM.
pub fn host() -> Option<Arc<dyn VmHost>> {
    HOST.get().cloned()
}

/// The published host or a uniform error for plugin code paths that must
/// surface failure as `Result`.
pub fn host_or_err() -> Result<Arc<dyn VmHost>, String> {
    host().ok_or_else(|| "caspar vm host is not initialised".to_string())
}

// ── Logging helpers (shared across plugins) ───────────────────────────────

thread_local! {
    static LOG_VM_CONTEXT: RefCell<String> = RefCell::new("main".to_string());
}

/// Bind the current thread's log lines to `vm_id`.
pub fn set_log_vm_context(vm_id: &str) {
    let next = if vm_id.trim().is_empty() {
        "main".to_string()
    } else {
        vm_id.trim().to_string()
    };
    LOG_VM_CONTEXT.with(|ctx| *ctx.borrow_mut() = next);
}

fn current_log_vm_context() -> String {
    LOG_VM_CONTEXT.with(|ctx| ctx.borrow().clone())
}

/// Emit a `vmLog` packet for an explicit VM id.
pub fn log_vm(text: String, vm_id: String, log_type: &str) {
    if let Some(h) = host() {
        let _ = h.dispatch(&json!({
            "key": "vmLog",
            "input": {
                "text": text,
                "data": text,
                "vmId": vm_id,
                "logType": log_type,
            }
        }));
    }
}

/// Emit a runtime `vmLog` packet for the thread's current VM context.
pub fn log(text: String) {
    let vm_id = current_log_vm_context();
    log_vm(text, vm_id, "runtime");
}
