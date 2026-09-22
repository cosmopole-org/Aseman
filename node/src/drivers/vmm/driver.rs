//! The node's side of workloads (P5-06, ADR 0030): [`NodeWorkloads`].
//!
//! Signals to a program are delivered to its workloads on the node's VMM, after
//! the node's own routing (proxy entities, proxied responses). HTTP ingress reaches
//! a workload through the VMM, or, for a runtime without ingress, becomes a signal
//! as it always did. Resource locks and the guest CRUD host actions are node state.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::drivers::vmm::globals::ResourceLockRegistry;
use crate::models::core::ICore;
use crate::models::ports::signaler::Listener;
use crate::models::ports::workloads::IWorkloads;
use crate::models::transaction::ITrx;
use crate::shell::api::model::{Creature, Store};
use crate::shell::api::packets::stores;

pub struct NodeWorkloads {
    pub(super) app: Arc<dyn ICore>,
    /// resource_id → per-resource lock state (the `lockResource` host call). The
    /// registry reaps idle entries so a guest cannot pin one lock per distinct
    /// `resource_id` for the life of the node.
    pub(crate) resource_locks: ResourceLockRegistry,
    /// The HTTP ingress server, owned here and reached through `tools().workloads()`.
    pub(crate) http_ingress: Arc<crate::drivers::vmm::network::ingress::VmHttpIngress>,
}

impl NodeWorkloads {
    pub fn new(app: Arc<dyn ICore>) -> Arc<NodeWorkloads> {
        // Publish the core handle so stateless host-call handlers can reach the
        // signaler and storage tools.
        crate::drivers::vmm::globals::set_global_app(app.clone());
        let http_ingress = crate::drivers::vmm::network::ingress::VmHttpIngress::new(app.clone());
        Arc::new(NodeWorkloads {
            app,
            resource_locks: ResourceLockRegistry::new(),
            http_ingress,
        })
    }
}

/// The runtime of a program entity: the entity's type when it has one, else the
/// program's runtime.
fn entity_runtime(app: &Arc<dyn ICore>, program: &str, entity: &str) -> String {
    let slot = Arc::new(Mutex::new(String::new()));
    let out = slot.clone();
    let program = program.to_owned();
    let entity = entity.to_owned();
    app.modify_state(
        true,
        Box::new(move |trx: &dyn ITrx| {
            let record = (crate::shell::api::model::program_ports::ProgramPorts { trx })
                .program_or_empty(&program);
            let mut runtime = record.runtime.trim().to_lowercase();
            if !entity.is_empty() {
                if let Ok(Some(found)) = aseman_ports::EntityDirectory::entity(
                    &crate::shell::api::model::entity_ports::EntityPorts {
                        trx,
                        blobs: &crate::drivers::blob_store::StorageRootBlobStore::new(""),
                    },
                    &program,
                    &entity,
                ) {
                    if !found.entity_type.trim().is_empty() {
                        runtime = found.entity_type.trim().to_lowercase();
                    }
                }
            }
            *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = runtime;
            Ok(())
        }),
    );
    let runtime = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    runtime
}

/// Deliver one signal to an entity of `program` through the node's VMM.
fn deliver(app: &Arc<dyn ICore>, program: &str, entity: &str, store_id: &str, packet: Value) {
    let Some(remote) = crate::shell::workloads::remote() else {
        eprintln!("signal to {program}/{entity} dropped: this node has no VMM (ASEMAN_VMM_ENDPOINT)");
        return;
    };
    let runtime = entity_runtime(app, program, entity);
    remote.signal(app, program, entity, &runtime, store_id, packet);
}

/// For a runtime without HTTP ingress, legacy forwarding: the request becomes a
/// `creatures/signal` to the entity and the caller gets `202 Accepted`.
fn forward_as_signal(app: &Arc<dyn ICore>, request: &Value) -> Value {
    let program = request["programId"].as_str().unwrap_or("").trim();
    let entity = request["entityId"].as_str().unwrap_or("").trim();
    let http = json!({
        "kind": "httpRequest",
        "creatureId": request["creatureId"].as_str().unwrap_or(""),
        "programId": program,
        "entityId": entity,
        "method": request["method"].as_str().unwrap_or("GET"),
        "path": request["path"].as_str().unwrap_or("/"),
        "query": request["query"].as_str().unwrap_or(""),
        "headers": request["headers"].clone(),
        "bodyBase64": request["bodyBase64"].as_str().unwrap_or(""),
    });
    let signal = json!({"action": "single", "entityId": entity, "data": http.to_string()});
    deliver(app, program, entity, "", signal);
    json!({
        "ok": true,
        "status": 202,
        "headers": {"content-type": "application/json"},
        "body": json!({"ok": true, "forwarded": "signal", "programId": program, "entityId": entity}).to_string(),
    })
}

impl IWorkloads for NodeWorkloads {
    fn assign(&self, machine_id: &str) {
        let app = self.app.clone();
        let machine = machine_id.to_string();
        let listener = Arc::new(Listener {
            id: machine_id.to_string(),
            paused: false,
            dis_time: 0,
            signal: Arc::new(move |key, value| {
                if key != "creatures/signal" {
                    return;
                }
                let raw = serde_json::to_vec(&value).unwrap_or_default();
                let packet = serde_json::from_slice::<stores::Send>(&raw).ok();
                let entity_id = packet
                    .as_ref()
                    .map(|packet| packet.entity_id.clone())
                    .unwrap_or_default();
                let store_id = packet
                    .as_ref()
                    .map(|packet| packet.store.id.clone())
                    .unwrap_or_default();
                // Proxied response: routed back to the original sender through the
                // proxy entity instead of running anything here.
                if crate::drivers::vmm::proxy::try_route_proxy_response(&app, &machine, &value) {
                    return;
                }
                // Proxy entity request: forwarded to its target.
                if crate::drivers::vmm::proxy::try_forward_through_proxy(
                    &app, &machine, &entity_id, &value,
                ) {
                    return;
                }
                deliver(&app, &machine, &entity_id, &store_id, value);
            }),
        });
        self.app.tools().signaler().listen_to_single(listener);
    }

    fn run_vm_entity(&self, machine_id: &str, store_id: &str, data: &str, entity_id: &str) {
        // The program must be a member of the store it runs in.
        let store_slot = Arc::new(Mutex::new(Store::default()));
        let member_slot = Arc::new(Mutex::new(false));
        let store_out = store_slot.clone();
        let member_out = member_slot.clone();
        let store_id_owned = store_id.to_string();
        let machine_owned = machine_id.to_string();
        self.app.modify_state(
            true,
            Box::new(move |trx: &dyn ITrx| {
                *store_out.lock().unwrap() = (crate::shell::api::model::store_ports::StorePorts { trx })
                    .store_or_empty(&store_id_owned);
                let ports = crate::shell::api::model::store_ports::MembershipPorts { trx };
                *member_out.lock().unwrap() =
                    aseman_ports::StoreAccess::is_member(&ports, &store_id_owned, &machine_owned)
                        .unwrap_or(false);
                Ok(())
            }),
        );
        if !*member_slot.lock().unwrap() {
            return;
        }
        let send = stores::Send {
            user: Creature::default(),
            store: store_slot.lock().unwrap().clone(),
            action: "single".to_string(),
            data: data.to_string(),
            entity_id: entity_id.to_string(),
            ..Default::default()
        };
        deliver(
            &self.app,
            machine_id,
            entity_id,
            store_id,
            serde_json::to_value(&send).unwrap_or_default(),
        );
    }

    fn start_http_ingress(&self, port: i64) {
        self.http_ingress.listen(port);
    }

    fn forward_http(&self, request: &Value) -> Value {
        let Some(remote) = crate::shell::workloads::remote() else {
            return json!({"ok": false, "status": 503, "error": "this node has no VMM"});
        };
        match remote.forward_http(request) {
            Ok(answer) => answer,
            Err(error) if error.to_string().contains("unsupported") => {
                forward_as_signal(&self.app, request)
            }
            Err(error) => json!({"ok": false, "status": 502, "error": error.to_string()}),
        }
    }

    fn resolve_http_route(&self, username: &str, path: &str) -> Option<Value> {
        use crate::drivers::vmm::http_route;

        let username = username.trim();
        if username.is_empty() {
            return None;
        }
        // Longest-prefix candidates over the leading request segments, capped so
        // the per-request work is bounded regardless of path length.
        let segments: Vec<String> = path
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        if segments.is_empty() {
            return None;
        }

        // Resolve username → creature id and match a registered route inside a
        // single state read.
        let result_slot = Arc::new(Mutex::new(None::<Value>));
        let result_clone = result_slot.clone();
        let username_owned = username.to_string();
        let segments_owned = segments.clone();
        self.app.modify_state(
            true,
            Box::new(move |trx: &dyn ITrx| {
                // The leading segment addresses the owning creature either by its
                // username (resolved through the index) or — because a username
                // qualified with a URL-shaped node source (e.g.
                // `name@http://host:port`) cannot be placed in a URL path — by its
                // creature id directly (e.g. `7@global`, which is path-safe). Try
                // the username index first, then fall back to treating the segment
                // itself as the creature id. Routes are stored keyed by creature
                // id, so both address forms converge on the same lookup.
                let mut candidates: Vec<String> = Vec::new();
                let creatures = crate::shell::api::model::creature_ports::CreaturePorts { trx };
                if let Some(via_username) =
                    aseman_ports::CreatureDirectory::creature_id_by_username(
                        &creatures,
                        &username_owned,
                    )
                    .map_err(|error| anyhow::anyhow!("{error}"))?
                {
                    candidates.push(via_username);
                }
                // Bare username local part (e.g. `m-tool-github`) → creature id,
                // via the alias link written when the route was registered.
                let routes = crate::shell::api::model::gateway_ports::GatewayPorts { trx };
                if let Some(via_alias) =
                    aseman_ports::GatewayRoutes::alias(&routes, &username_owned)
                        .map_err(|error| anyhow::anyhow!("{error}"))?
                {
                    if !candidates.iter().any(|c| c == &via_alias) {
                        candidates.push(via_alias);
                    }
                }
                if !candidates.iter().any(|c| c == &username_owned) {
                    candidates.push(username_owned.clone());
                }
                let max = segments_owned.len().min(http_route::MAX_ROUTE_SEGMENTS);
                'outer: for creature_id in &candidates {
                    for take in (1..=max).rev() {
                        let prefix = segments_owned[..take].join("/");
                        let Some(route) =
                            aseman_ports::GatewayRoutes::route(&routes, creature_id, &prefix)
                                .map_err(|error| anyhow::anyhow!("{error}"))?
                        else {
                            continue;
                        };
                        let rest: Vec<&str> =
                            segments_owned[take..].iter().map(|s| s.as_str()).collect();
                        *result_clone.lock().unwrap() = Some(json!({
                            "creatureId": creature_id,
                            "programId": route.program_id,
                            "entityId": route.entity_id,
                            "vmId": route.pinned_vm_id,
                            "runtime": route.runtime,
                            "path": format!("/{}", rest.join("/")),
                        }));
                        break 'outer;
                    }
                }
                Ok(())
            }),
        );
        let out = result_slot.lock().unwrap().take();
        out
    }

    fn acquire_resource_lock(&self, resource_id: &str, owner_id: &str) -> Result<(), String> {
        self.resource_locks.acquire(resource_id, owner_id)
    }

    fn release_resource_lock(&self, resource_id: &str, owner_id: &str) -> Result<(), String> {
        self.resource_locks.release(resource_id, owner_id)
    }

    fn host_action_micro(&self, op: &str, input: &Value, req_id: i64) -> (String, i64) {
        self.handle_micro_host_action(op, input, req_id)
    }

    fn exec_shell_action(&self, caller: &str, input: &Value) -> String {
        self.handle_exec_shell_action(caller, input, 0).0
    }

    fn host_action_resource_store(&self, op: &str, input: &Value, req_id: i64) -> (String, i64) {
        self.handle_resource_store_crud(op, input, req_id)
    }

    fn host_action_resource_entity_create(&self, input: &Value, req_id: i64) -> (String, i64) {
        self.handle_resource_entity_create(input, req_id)
    }

    fn host_action_resource_entity_delete(&self, input: &Value, req_id: i64) -> (String, i64) {
        self.handle_resource_entity_delete(input, req_id)
    }

    fn host_action_store(&self, op: &str, input: &Value, req_id: i64) -> (String, i64) {
        self.handle_store_crud(op, input, req_id)
    }

    fn host_action_creature(&self, op: &str, input: &Value, req_id: i64) -> (String, i64) {
        self.handle_creature_crud(op, input, req_id)
    }

    fn host_action_program(&self, op: &str, input: &Value, req_id: i64) -> (String, i64) {
        self.handle_program_crud(op, input, req_id)
    }
}

/// `normalizeRuntime` — Go's `strings.ToLower(TrimSpace(.))`.
pub(super) fn normalize_runtime(runtime: &str) -> String {
    runtime.trim().to_lowercase()
}

/// Canonical key of the registered fallback runtime, used when a program
/// record carries no runtime of its own.
pub(super) fn default_runtime_key() -> String {
    caspar_vm_sdk::registry::default_key().unwrap_or_default()
}

/// Field-getter helper — emulates Go's generic `checkField[T]`.
pub(super) fn check_field<'a>(input: &'a Value, key: &str) -> Option<&'a Value> {
    input.get(key)
}

pub(super) fn check_str(input: &Value, key: &str, default: &str) -> String {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| default.to_string())
}

pub(super) fn check_i64(input: &Value, key: &str, default: i64) -> i64 {
    input
        .get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .unwrap_or(default)
}

pub(super) fn check_bool(input: &Value, key: &str, default: bool) -> bool {
    if let Some(v) = input.get(key) {
        if let Some(b) = v.as_bool() {
            return b;
        }
        if let Some(s) = v.as_str() {
            return s == "true" || s == "1";
        }
    }
    default
}

/// Convenience for `time.Now().UnixMilli()`.
pub(super) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

