use serde_json::Value as JsonValue;

/// The node's side of workloads (P5-06, ADR 0030).
///
/// Workloads run on the node's VMM (`shell::workloads`); this port holds only what
/// the node itself owns about them: delivering signals to programs, HTTP ingress and
/// custom gateway routes, the resource locks guests take, and the node-side guest
/// operations (creature, store, program, and resource CRUD) behind the guest API.
pub trait IWorkloads: Send + Sync {
    /// Register the signal listener that delivers `creatures/signal` events of
    /// `machine_id` (a program) to its workloads.
    fn assign(&self, machine_id: &str);
    /// Deliver `data` to an entity of `machine_id` (a woken alarm, a chain message).
    /// `entity_id` empty means the program's default entity.
    fn run_vm_entity(&self, machine_id: &str, store_id: &str, data: &str, entity_id: &str);

    /// Start the HTTP ingress listener (`/{creatureId}/{programId}/{entityId}/{vmId}/…`
    /// and custom routes). No-op when `port <= 0` or already running.
    fn start_http_ingress(&self, port: i64);
    /// Forward a packaged inbound HTTP request to the workload it targets.
    ///
    /// `request`: `{ creatureId, programId, entityId, vmId, method, path, query,
    /// headers, bodyBase64 }`; returns `{ ok, status, headers, bodyBase64 }`.
    fn forward_http(&self, request: &JsonValue) -> JsonValue;
    /// Resolve `/{creature}/{path…}` to the entity a deployer bound to that custom
    /// path: `{ creatureId, programId, entityId, vmId, runtime, path }`.
    fn resolve_http_route(&self, creature: &str, path: &str) -> Option<JsonValue>;

    /// Acquire an exclusive lock on `resource_id` for `owner_id` (FIFO; blocks).
    fn acquire_resource_lock(&self, resource_id: &str, owner_id: &str) -> Result<(), String>;
    fn release_resource_lock(&self, resource_id: &str, owner_id: &str) -> Result<(), String>;

    /// Dispatch a micro host action (genId, getLink, putJson, …).
    fn host_action_micro(&self, op: &str, input: &JsonValue, req_id: i64) -> (String, i64);
    /// Run a registered shell action for a guest. `caller` is the node-resolved
    /// creature behind the call.
    fn exec_shell_action(&self, caller: &str, input: &JsonValue) -> String;
    fn host_action_resource_store(&self, op: &str, input: &JsonValue, req_id: i64)
    -> (String, i64);
    fn host_action_resource_entity_create(&self, input: &JsonValue, req_id: i64) -> (String, i64);
    fn host_action_resource_entity_delete(&self, input: &JsonValue, req_id: i64) -> (String, i64);
    fn host_action_store(&self, op: &str, input: &JsonValue, req_id: i64) -> (String, i64);
    fn host_action_creature(&self, op: &str, input: &JsonValue, req_id: i64) -> (String, i64);
    fn host_action_program(&self, op: &str, input: &JsonValue, req_id: i64) -> (String, i64);
    /// A guest's `signal` to a store (identity stamped by the node).
    fn host_action_signal(&self, input: &JsonValue) -> String;
}
