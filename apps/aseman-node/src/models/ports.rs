//! Driver port traits — the interfaces the core depends on.
//!
//! The rate-limiter, security, signaler, storage, tools, workloads, and
//! network (transport) port contracts are defined inline; each is implemented
//! by a concrete driver under `crate::adapters`. `tools` bundles every port
//! into a single container handed to the core orchestrator.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use aseman_network_legacy::TlsConfig;
use dashmap::DashMap;
use serde_json::Value;
use serde_json::Value as JsonValue;

use crate::legacy::utils::compat::GoError;
use crate::models::packet::{LogPacket, LogQuery};
use crate::models::transaction::ITrx;

/// The client-facing transport a request arrived on.
///
/// The protocol never partitions the quota — an identity's tokens are shared
/// across every transport — but it is carried through the check so the limiter
/// can attribute rejections per-protocol for telemetry and logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// The length-prefixed TLS-TCP client transport.
    Tcp,
    /// The TLS WebSocket client transport.
    Ws,
    /// The VMM HTTP ingress that forwards requests to VM instances.
    Http,
}

impl Protocol {
    /// Stable lowercase label used in logs and telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Tcp => "tcp",
            Protocol::Ws => "ws",
            Protocol::Http => "http",
        }
    }
}

/// Identifies who is making a request, so the limiter can pick the right
/// bucket and tier.
///
/// The limiter derives the bucket key from **verified** identity only:
///
/// * `user_id` must be an authenticated user (e.g. the id a transport has
///   pinned onto its socket *after* a successful signature check), never a
///   value merely claimed in an unverified packet. When it is non-empty the
///   request is billed to that user under the authenticated tier.
/// * otherwise the request is anonymous and billed to `peer_ip` under the
///   (tighter) anonymous tier.
///
/// Keeping spoofable, unverified identifiers out of the key is what makes the
/// limiter evasion-resistant: an attacker cannot mint fresh buckets by
/// rotating a forged user id, because a forged id is never trusted here.
#[derive(Debug, Clone)]
pub struct RateLimitKey {
    /// Transport the request arrived on (telemetry only).
    pub protocol: Protocol,
    /// Verified authenticated user id, or empty for anonymous traffic.
    pub user_id: String,
    /// Remote peer IP (best-effort; may be empty for in-process callers).
    pub peer_ip: String,
    /// Action path / request target being invoked (telemetry only).
    pub path: String,
}

impl RateLimitKey {
    /// Build a key for a verified-authenticated request.
    pub fn authenticated(protocol: Protocol, user_id: &str, peer_ip: &str, path: &str) -> Self {
        RateLimitKey {
            protocol,
            user_id: user_id.to_string(),
            peer_ip: peer_ip.to_string(),
            path: path.to_string(),
        }
    }

    /// Build a key for anonymous (pre-auth) traffic, billed to the peer IP.
    pub fn anonymous(protocol: Protocol, peer_ip: &str, path: &str) -> Self {
        RateLimitKey {
            protocol,
            user_id: String::new(),
            peer_ip: peer_ip.to_string(),
            path: path.to_string(),
        }
    }

    /// True when the request carries a verified authenticated identity.
    pub fn is_authenticated(&self) -> bool {
        !self.user_id.is_empty()
    }
}

/// Which limit rejected a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitScope {
    /// The per-identity (per-user or per-IP) bucket was exhausted.
    Identity,
    /// The node-wide aggregate limiter was exhausted.
    Global,
}

impl LimitScope {
    pub fn as_str(self) -> &'static str {
        match self {
            LimitScope::Identity => "identity",
            LimitScope::Global => "global",
        }
    }
}

/// The outcome of a single [`IRateLimiter::check`] call.
#[derive(Debug, Clone)]
pub enum RateLimitDecision {
    /// The request may proceed. `remaining` is the whole tokens left in the
    /// identity bucket after this request (best-effort, for `X-RateLimit-*`
    /// style headers).
    Allowed { remaining: u32 },
    /// The request is rejected. `retry_after` is the minimum wait before a
    /// retry could succeed against the exhausted bucket; `scope` says which
    /// limit tripped.
    Limited {
        retry_after: Duration,
        scope: LimitScope,
    },
}

impl RateLimitDecision {
    /// Convenience: whether the request was allowed through.
    pub fn is_allowed(&self) -> bool {
        matches!(self, RateLimitDecision::Allowed { .. })
    }
}

/// A point-in-time snapshot of limiter counters for telemetry.
#[derive(Debug, Clone, Default)]
pub struct RateLimiterSnapshot {
    /// Whether enforcement is currently on.
    pub enabled: bool,
    /// Number of live per-identity buckets currently tracked.
    pub tracked_identities: u64,
    /// Total requests admitted since start.
    pub allowed: u64,
    /// Total requests rejected by a per-identity bucket since start.
    pub limited_identity: u64,
    /// Total requests rejected by the global limiter since start.
    pub limited_global: u64,
}

/// Response code the length-prefixed client transports (TCP / WS) return when
/// a request is throttled. Chosen distinct from the existing action codes
/// (`0` ok, `1` not-found, `2` parse-error, `3` act-error, `4` auth-failed) so
/// clients can special-case a back-off without ambiguity. Mirrors HTTP `429`.
pub const RATE_LIMITED_RES_CODE: i64 = 8;

/// Build the JSON body handed back to a throttled client. Includes the machine
/// -readable `message`, the `retryAfterMs` a client should wait, and which
/// `scope` tripped. Old clients that only read `message` still see
/// `"rate_limited"`.
pub fn rate_limited_body(retry_after: Duration, scope: LimitScope) -> serde_json::Value {
    serde_json::json!({
        "message": "rate_limited",
        "retryAfterMs": retry_after.as_millis() as u64,
        "scope": scope.as_str(),
    })
}

/// Protocol-agnostic admission control for client → node requests.
pub trait IRateLimiter: Send + Sync {
    /// Consume one unit of quota for `key` and report whether the request may
    /// proceed. Implementations must be safe to call concurrently from many
    /// transport threads.
    fn check(&self, key: &RateLimitKey) -> RateLimitDecision;

    /// Whether enforcement is currently active. When `false`, [`check`](Self::check)
    /// always returns [`RateLimitDecision::Allowed`].
    fn enabled(&self) -> bool;

    /// Cheap snapshot of counters for telemetry / diagnostics.
    fn snapshot(&self) -> RateLimiterSnapshot;
}

/// The security driver interface — key management, encryption, auth.
pub trait ISecurity: Send + Sync {
    fn load_keys(&self);
    fn generate_secure_key_pair(&self, tag: &str);
    fn fetch_key_pair(&self, tag: &str) -> Vec<Vec<u8>>;
    fn encrypt(&self, tag: &str, plain_text: &str) -> String;
    fn decrypt(&self, tag: &str, cipher_text: &str) -> String;
    /// Returns `(authenticated, resolvedUserId, isGod)`.
    fn auth_with_signature(
        &self,
        user_id: &str,
        packet: &[u8],
        signature_base64: &str,
    ) -> (bool, String, bool);
    fn has_access_to_store(&self, user_id: &str, store_id: &str) -> bool;
}

/// Callback invoked when a signal fires. Receives the signal key and payload.
pub type SignalFn = Arc<dyn Fn(String, Value) + Send + Sync>;

/// Callback invoked on group join/leave. Receives the group id and user id.
pub type JoinFn = Arc<dyn Fn(String, String) + Send + Sync>;

/// A group of stores sharing a single listener.
///
/// `listener` and `override_` are mutated after construction (see
/// `ISignaler::listen_to_group`), so they live behind a `Mutex` to stay
/// thread-safe.
pub struct Group {
    pub stores: Arc<DashMap<String, String>>,
    pub listener: std::sync::Mutex<Option<Arc<Listener>>>,
    pub override_: std::sync::Mutex<bool>,
}

/// A single signal listener.
#[derive(Clone)]
pub struct Listener {
    pub id: String,
    pub paused: bool,
    pub dis_time: i64,
    pub signal: SignalFn,
}

/// A listener that bridges every signal globally.
#[derive(Clone)]
pub struct GlobalListener {
    pub signal: SignalFn,
}

/// A listener for group join/leave events.
#[derive(Clone)]
pub struct JoinListener {
    pub join: JoinFn,
    pub leave: JoinFn,
}

/// The signaler driver interface — realtime pub/sub fan-out.
pub trait ISignaler: Send + Sync {
    fn lock(&self);
    fn unlock(&self);
    fn listeners(&self) -> Arc<DashMap<String, Arc<Listener>>>;
    fn groups(&self) -> Arc<DashMap<String, Arc<Group>>>;
    fn listen_to_single(&self, listener: Arc<Listener>);
    fn listen_to_group(&self, listener: Arc<Listener>, override_functionaly: bool);
    fn brdige_globally(&self, listener: Arc<GlobalListener>, override_functionaly: bool);
    fn listen_to_join(&self, listener: Arc<JoinListener>);
    fn signal_user(&self, key: &str, listener_id: &str, data: Value, pack: bool);
    fn signal_group(
        &self,
        key: &str,
        group_id: &str,
        data: Value,
        pack: bool,
        exceptions: Vec<String>,
    );
    /// Fan a signal out to every current member of a store.
    ///
    /// Membership is resolved from the state's `onaccess::<store>::<member>`
    /// grants at the moment of delivery — NOT from the in-memory group
    /// registry. The registry is populated when a connection authenticates (and
    /// when a program boots), so a store a member gained access to *after* that
    /// point is not in it, and a signal on that store would reach nobody until
    /// the member reconnected. A store's membership is state; reading it from
    /// state is the only way the fan-out cannot go stale.
    ///
    /// Only members whose grant carries `read` are delivered to — the same flag
    /// `stores/history` requires — so what a member is pushed live and what they
    /// may replay never diverge.
    ///
    /// `exceptions` are member ids to skip (the sender). `federate` pushes the
    /// same packet to each peer origin holding a member; pass `false` when
    /// re-emitting a packet that already arrived over federation, so it is not
    /// bounced back around the network.
    fn signal_store(
        &self,
        key: &str,
        store_id: &str,
        data: Value,
        exceptions: Vec<String>,
        federate: bool,
    );
    fn join_group(&self, group_id: &str, user_id: &str);
    fn leave_group(&self, group_id: &str, user_id: &str);
    /// Remove `user_id` from every group it is a member of, reaping any group
    /// left with no members, listener, or override. Called when a user's last
    /// connection closes so a disconnected client's group memberships (and the
    /// now-empty groups they leave behind) cannot accumulate for the life of
    /// the node.
    fn leave_all_groups(&self, user_id: &str);
    fn retrive_group(&self, group_id: &str) -> Option<Arc<Group>>;
}

/// Key/value database handle — the legacy provider's store seam.
pub type KvDb = Arc<dyn aseman_storage_legacy::LegacyKvStore>;

/// The storage driver interface.
pub trait IStorage: Send + Sync {
    fn storage_root(&self) -> String;
    fn kv_db(&self) -> KvDb;
    fn gen_id(&self, t: &dyn ITrx, origin: &str) -> String;
    /// Append one signal packet to the store's time-series log. `tags` are the
    /// sender's labels, already validated by the caller; they are stored with
    /// the packet so [`IStorage::read_store_logs`] can filter on them.
    ///
    /// Errors rather than reporting a packet it did not write: this row is the
    /// message, so a caller must be able to tell the sender their message did
    /// not land instead of watching it vanish on the next read.
    fn log_time_sieries(
        &self,
        store_id: &str,
        user_id: &str,
        data: &str,
        tags: &[String],
        time_val: i64,
    ) -> Result<LogPacket>;
    fn update_log(
        &self,
        store_id: &str,
        user_id: &str,
        signal_id: &str,
        data: &str,
        time_val: i64,
    ) -> LogPacket;
    /// Read a store's persisted signals, newest first, filtered by the
    /// query's tags and time bounds.
    ///
    /// Errors rather than returning an empty page: "the log is unreachable" and
    /// "this store has nothing to say" must not look the same to a reader.
    fn read_store_logs(&self, store_id: &str, query: &LogQuery) -> Result<Vec<LogPacket>>;
    fn pick_store_logs(&self, store_id: &str, ids: Vec<String>) -> Vec<LogPacket>;
}

/// Aggregates every node driver behind a single interface.
pub trait ITools: Send + Sync {
    fn security(&self) -> Arc<dyn ISecurity>;
    fn signaler(&self) -> Arc<dyn ISignaler>;
    fn storage(&self) -> Arc<dyn IStorage>;
    fn network(&self) -> Arc<dyn INetwork>;
    fn workloads(&self) -> Arc<dyn IWorkloads>;
    /// The shared, protocol-agnostic client-request rate limiter. Every
    /// client-facing transport consults this single instance so a client's
    /// quota is unified across TCP, WebSocket, and the HTTP ingress.
    fn rate_limiter(&self) -> Arc<dyn IRateLimiter>;
}

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

/// Network port traits — the four transports plus the top-level
/// [`INetwork`] facade that bundles them. Compatibility TLS material is
/// owned by `aseman-network-legacy` with its wire implementation.
/// The top-level network driver interface.
pub trait INetwork: Send + Sync {
    fn chain(&self) -> Arc<dyn IChain>;
    fn federation(&self) -> Arc<dyn IFederation>;
    fn tcp(&self) -> Arc<dyn ITcp>;
    fn ws(&self) -> Arc<dyn IWs>;
    fn tls_config(&self) -> Option<TlsConfig>;
    fn run(&self, ports: HashMap<String, i64>);
}
/// Pipeline callback. Receives a batch of payloads and a per-payload
/// emit callback; returns the keys of the messages that were forwarded.
pub type PipelineFn =
    Box<dyn Fn(Vec<Vec<u8>>, Box<dyn Fn(Vec<u8>) + Send + Sync>) -> Vec<String> + Send + Sync>;
/// The blockchain network driver interface.
pub trait IChain: Send + Sync {
    fn listen(&self, port: i64, tls_config: Option<TlsConfig>);
    fn restore_from_storage(&self);
    fn submit_trx(&self, chain_id: &str, machine_id: &str, typ: &str, payload: Vec<u8>);
    fn register_pipeline(&self, pipeline: PipelineFn);
    fn notify_new_machine_created(&self, chain_id: &str, machine_id: &str);
    fn create_temp_chain(&self, store_id: &str) -> String;
    fn create_work_chain(&self, store_id: &str) -> String;
    fn create_shard_chain(
        &self,
        chain_id: &str,
        shard_chain_id: &str,
        peers: Vec<String>,
    ) -> String;
    fn peers(&self) -> Vec<String>;
    fn user_owns_origin(&self, user_id: &str, origin: &str) -> bool;
    fn get_node_owner_id(&self, origin: &str) -> String;
    fn close(&self);

    /// Submit a chain packet onto a chain (the chain module owns the outbound
    /// submission queue and framing; this is the sole entry point).
    fn submit_chain_op(&self, chain_id: &str, op: crate::legacy::globe::ChainPacketOp);

    /// The consensus provider installed as this chain's application handler, if
    /// any. Ownership lives with the chain module, not the core orchestrator.
    fn consensus_provider(&self) -> Option<Arc<dyn aseman_ports::consensus::ConsensusProvider>> {
        None
    }

    /// Register a chain base-request response callback (chain-module-owned).
    fn register_chain_callback(
        &self,
        callback_id: &str,
        callback: crate::models::chain::ChainCallback,
    );
    /// Park a no-op chain callback only if one is not already registered (the
    /// real callback registered by the sender must win).
    fn park_chain_callback(&self, callback_id: &str);
    /// Remove and return a chain base-request response callback.
    fn take_chain_callback(
        &self,
        callback_id: &str,
    ) -> Option<Arc<crate::models::chain::ChainCallback>>;
    /// Register a typed-message reply callback (chain-module-owned).
    fn register_message_callback(
        &self,
        callback_id: &str,
        callback: crate::models::chain::MessageCallback,
    );
    /// Remove and return a typed-message reply callback.
    fn take_message_callback(
        &self,
        callback_id: &str,
    ) -> Option<Arc<crate::models::chain::MessageCallback>>;
}
/// Callback delivering a federation response — payload, status code, error.
pub type FedRequestCallback = Box<dyn Fn(Vec<u8>, i64, Option<GoError>) + Send + Sync>;
/// The federation (inter-organization) network driver interface.
pub trait IFederation: Send + Sync {
    fn listen(&self, port: i64, tls_config: Option<TlsConfig>);
    fn send_fed_request(
        &self,
        dest_org: &str,
        request_id: &str,
        user_id: &str,
        path: &str,
        payload: Vec<u8>,
        signature: &str,
    );
    fn send_fed_response(&self, dest_org: &str, request_id: &str, res_code: i64, res: Value);
    fn send_fed_update(
        &self,
        dest_org: &str,
        key: &str,
        update_pack: Value,
        target_type: &str,
        target_id_val: &str,
        exceptions: Vec<String>,
    );
    #[allow(clippy::too_many_arguments)]
    fn send_fed_request_by_callback(
        &self,
        dest_org: &str,
        request_id: &str,
        user_id: &str,
        path: &str,
        payload: Vec<u8>,
        signature: &str,
        callback: FedRequestCallback,
    );
}
/// The raw TCP client API driver interface.
pub trait ITcp: Send + Sync {
    fn listen(&self, port: i64, tls_config: Option<TlsConfig>);
}
/// The WebSocket client API driver interface.
pub trait IWs: Send + Sync {
    fn listen(&self, port: i64, tls_config: Option<TlsConfig>);
}
