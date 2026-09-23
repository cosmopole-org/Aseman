//! Translation of `core/module/core/core.go` — the `Core` orchestrator.
//!
//! `Core` is the `ICore` implementation, the central object that gives every
//! action / driver access to the rest of the system. It owns the `ITools`
//! bundle (storage, security, signaler, network, vmm), the `IActor`
//! registry, the `IGlobe` validator-set coordinator, and the chain dispatch
//! channel.
//!
//! The chain dispatch goroutine, the election ticker, and the chain-packet
//! callbacks all stay as background threads spawned by `Load`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use aseman_config::AsemanConfig;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rsa::pkcs1v15::SigningKey as Pkcs1v15SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::pss::SigningKey as PssSigningKey;
use rsa::rand_core::OsRng;
use rsa::sha2::Sha256;
use rsa::signature::{RandomizedSigner, SignatureEncoding};
use rsa::RsaPrivateKey;
use serde_json::{json, Value};

use crate::core::actor::model::trx::TrxWrapper;
use crate::core::actor::{Actor, Info as BaseInfo, State as ActorState};
use crate::core::globe::{ChainPacketOp, Globe};
use crate::drivers::network::chain::Blockchain;
use crate::drivers::network::federation::FedNet;
use crate::drivers::network::framing::tls_config_from_files;
use crate::drivers::network::Network as NetworkDriver;
use crate::drivers::security::Security;
use crate::drivers::signaler::Signaler;
use crate::drivers::storage::Storage;
use crate::drivers::vmm::NodeWorkloads;
use crate::models::action::actor::IActor;
use crate::models::action::TrxClosure;
use crate::models::chain::{
    ChainBaseRequest, ChainCallback, ChainElectionPacket, ChainMessage, ChainPayPacket,
    ChainResponse, ChainStakePacket, MessageCallback,
};
use crate::models::core::{ICore, StateClosure};
use crate::models::globe::IGlobe;
use crate::models::info::IInfo;
use crate::models::ports::network::INetwork;
use crate::models::ports::ratelimit::IRateLimiter;
use crate::models::ports::security::ISecurity;
use crate::models::ports::signaler::ISignaler;
use crate::models::ports::storage::IStorage;
use crate::models::ports::tools::ITools;
use crate::models::ports::workloads::IWorkloads;
use crate::models::transaction::ITrx;
use crate::models::worker::Trx as WorkerTrx;
use crate::shell::api::model::Program;
use crate::shell::api::packets::creatures::ConsumeLockInput;
use crate::shell::utils::crypto::secure_unique_string;
use crate::util::GoError;

const MAX_VALIDATOR_COUNT: usize = 50;
const ELECTION_COMMIT_SECONDS: i64 = 120;
const ELECTION_REVEAL_SECONDS: i64 = 120;

/// Tools — aggregates every driver behind a single `ITools` impl.
pub struct Tools {
    security: Arc<dyn ISecurity>,
    signaler: Arc<dyn ISignaler>,
    storage: Arc<dyn IStorage>,
    network: Arc<dyn INetwork>,
    vmm: Arc<dyn IWorkloads>,
    rate_limiter: Arc<dyn IRateLimiter>,
}

impl ITools for Tools {
    fn security(&self) -> Arc<dyn ISecurity> {
        self.security.clone()
    }
    fn signaler(&self) -> Arc<dyn ISignaler> {
        self.signaler.clone()
    }
    fn storage(&self) -> Arc<dyn IStorage> {
        self.storage.clone()
    }
    fn network(&self) -> Arc<dyn INetwork> {
        self.network.clone()
    }
    fn workloads(&self) -> Arc<dyn IWorkloads> {
        self.vmm.clone()
    }
    fn rate_limiter(&self) -> Arc<dyn IRateLimiter> {
        self.rate_limiter.clone()
    }
}

/// Submission envelope routed onto the chain dispatch channel.
#[derive(Clone)]
struct ChainSubmission {
    chain_id: String,
    op: ChainPacketOp,
}

/// The Caspar node orchestrator implementing [`ICore`].
pub struct Core {
    config: Option<Arc<AsemanConfig>>,
    owner_id: String,
    owner_priv_key: Arc<RsaPrivateKey>,
    id: String,
    ip: String,

    actor: Arc<dyn IActor>,
    state: Mutex<CoreState>,
    tools: Mutex<Option<Arc<dyn ITools>>>,
    globe: Mutex<Option<Arc<dyn IGlobe>>>,
    chain_tx: Mutex<Option<crossbeam_channel::Sender<ChainSubmission>>>,
    started: Mutex<bool>,
    callbacks: Mutex<HashMap<String, Arc<ChainCallback>>>,
    message_callbacks: Mutex<HashMap<String, Arc<MessageCallback>>>,

    gods: Mutex<Vec<String>>,
    free_nodes: Mutex<HashMap<String, bool>>,
    app_pending_trxs: Mutex<Vec<WorkerTrx>>,
    elections: Mutex<Vec<crate::models::chain::Election>>,
    cost: Mutex<CostConfig>,
    priv_key: Mutex<Option<Arc<RsaPrivateKey>>>,
}

#[derive(Default)]
struct CoreState {
    elec_starter: String,
    elec_start_time: i64,
    elec_reg: bool,
}

#[derive(Default, Clone)]
struct CostConfig {
    execution_cost_per_second: i64,
    vm_ram_cost_per_mb_minute: i64,
    vm_cpu_core_cost_per_minute: i64,
    vm_disk_cost_per_gb_minute: i64,
}

impl Core {
    /// `NewCore(origin, ownerId, ownerPrivateKey)`.
    pub fn new(origin: &str, owner_id: &str, owner_priv_key: Arc<RsaPrivateKey>) -> Arc<Core> {
        Self::new_inner(origin, owner_id, owner_priv_key, None)
    }

    pub fn new_configured(
        origin: &str,
        owner_id: &str,
        owner_priv_key: Arc<RsaPrivateKey>,
        config: Arc<AsemanConfig>,
    ) -> Arc<Core> {
        Self::new_inner(origin, owner_id, owner_priv_key, Some(config))
    }

    fn new_inner(
        origin: &str,
        owner_id: &str,
        owner_priv_key: Arc<RsaPrivateKey>,
        config: Option<Arc<AsemanConfig>>,
    ) -> Arc<Core> {
        let mut free_nodes = HashMap::new();
        if let Some(root) = config
            .as_ref()
            .and_then(|config| config.core.root_node.as_ref())
        {
            free_nodes.insert(root.clone(), true);
        }
        Arc::new(Core {
            config,
            owner_id: owner_id.to_string(),
            owner_priv_key,
            id: origin.to_string(),
            ip: origin.to_string(),
            actor: Arc::new(Actor::new()),
            state: Mutex::new(CoreState::default()),
            tools: Mutex::new(None),
            globe: Mutex::new(None),
            chain_tx: Mutex::new(None),
            started: Mutex::new(false),
            callbacks: Mutex::new(HashMap::new()),
            message_callbacks: Mutex::new(HashMap::new()),
            gods: Mutex::new(Vec::new()),
            free_nodes: Mutex::new(free_nodes),
            app_pending_trxs: Mutex::new(Vec::new()),
            elections: Mutex::new(Vec::new()),
            cost: Mutex::new(CostConfig::default()),
            priv_key: Mutex::new(None),
        })
    }

    pub fn mark_as_started(&self) {
        *self.started.lock().unwrap() = true;
    }

    fn parse_private_key(pem_bytes: &[u8]) -> Result<RsaPrivateKey> {
        let s = std::str::from_utf8(pem_bytes)?;
        Ok(RsaPrivateKey::from_pkcs8_pem(s)?)
    }

    /// Sign `data` with the given RSA key using PSS-SHA256 + the same salt
    /// length Go used (`PSSSaltLengthEqualsHash`).
    fn sign_with(key: &RsaPrivateKey, data: &[u8]) -> String {
        let _ = Pkcs1v15SigningKey::<Sha256>::new(key.clone());
        let signing_key = PssSigningKey::<Sha256>::new(key.clone());
        let sig = signing_key.sign_with_rng(&mut OsRng, data);
        B64.encode(sig.to_bytes())
    }

    fn chain_message_targets_local(&self, packet: &ChainMessage) -> bool {
        packet.recievers.contains_key("*") || packet.recievers.contains_key(&self.id)
    }

    fn chain_message_machine_ids(&self, packet: &ChainMessage) -> HashMap<String, bool> {
        let mut machine_ids = HashMap::new();
        if let Some(map) = packet.recievers.get(&self.id) {
            for k in map.keys() {
                machine_ids.insert(k.clone(), true);
            }
        }
        if let Some(pay) = &packet.pay {
            for m in &pay.machine_ids {
                machine_ids.insert(m.clone(), true);
            }
        }
        machine_ids
    }

    fn run_chain_message(self: &Arc<Self>, packet: ChainMessage) {
        let machine_ids = self.chain_message_machine_ids(&packet);
        for machine_id in machine_ids.keys() {
            let runtime_slot = Arc::new(Mutex::new(String::new()));
            let runtime_clone = runtime_slot.clone();
            let machine_id_owned = machine_id.clone();
            self.modify_state(
                true,
                Box::new(move |trx: &dyn ITrx| {
                    let vm = (crate::shell::api::model::program_ports::ProgramPorts { trx })
                        .program_or_empty(&machine_id_owned.clone());
                    *runtime_clone.lock().unwrap() = vm.runtime;
                    Ok(())
                }),
            );
            let runtime_type = runtime_slot.lock().unwrap().clone();
            let offered = crate::shell::workloads::remote()
                .is_some_and(|remote| remote.offers(&runtime_type));
            if !offered {
                continue;
            }
            // The program's signal listener delivers the message to its workloads;
            // without one (no assign yet), it goes to the entity directly.
            let listener = self
                .tools()
                .signaler()
                .listeners()
                .get(machine_id)
                .map(|e| e.value().clone());
            let payload = packet.payload.clone();
            match listener {
                Some(listener) => {
                    thread::spawn(move || {
                        let value = Value::String(String::from_utf8_lossy(&payload).into_owned());
                        (listener.signal)("creatures/signal".to_string(), value);
                    });
                }
                None => {
                    let machine_id_owned = machine_id.clone();
                    let store_id = packet.store_id.clone();
                    let trans = self.clone();
                    thread::spawn(move || {
                        let data = String::from_utf8_lossy(&payload).into_owned();
                        trans.tools().workloads().run_vm_entity(
                            &machine_id_owned,
                            &store_id,
                            &data,
                            "",
                        );
                    });
                }
            }
        }
    }

    fn consume_pay_lock_on_chain(self: &Arc<Self>, pay: Option<&ChainPayPacket>) -> bool {
        let Some(pay) = pay else {
            return false;
        };
        if pay.lock_id.is_empty()
            || pay.user_id.is_empty()
            || pay.lock_signature.is_empty()
            || pay.amount <= 0
        {
            return false;
        }
        let Some(globe) = self.globe.lock().unwrap().clone() else {
            return false;
        };
        let input = ConsumeLockInput {
            typ: "pay".to_string(),
            user_id: pay.user_id.clone(),
            lock_id: pay.lock_id.clone(),
            signature: pay.lock_signature.clone(),
            amount: pay.amount,
            step: None,
        };
        let inp = serde_json::to_vec(&input).unwrap_or_default();
        let sign = self.sign_packet_as_owner(&inp);
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        let owner = self.owner_id.clone();
        let cb: crate::models::globe::BaseResponseCallback =
            Box::new(move |_data: Vec<u8>, status: i64, err: Option<GoError>| {
                let ok = err.is_none() && status < 400;
                let _ = tx.send(ok);
            });
        globe.send_base_request_on_chain("/creatures/consumeLock", inp, &sign, &owner, "", cb);
        rx.recv_timeout(Duration::from_secs(30)).unwrap_or(false)
    }

    fn handle_chain_packet(self: &Arc<Self>, typ: &str, trx_payload: &[u8]) -> String {
        if let Some(globe) = self.globe.lock().unwrap().clone() {
            if globe.handle(typ, trx_payload.to_vec()) {
                return String::new();
            }
        }
        match typ {
            "message" => {
                let packet: ChainMessage = match serde_json::from_slice(trx_payload) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("handle_chain_packet: bad message: {}", e);
                        return String::new();
                    }
                };
                let mut packet = packet;
                if packet.message_type.is_empty() {
                    packet.message_type = "vm.execute".to_string();
                }
                if !packet.reply_to.is_empty() {
                    // One-shot: a reply delivers its callback exactly once, so
                    // remove it here. Leaving it in the map (the old `get`) meant
                    // every registered message callback lived for the life of the
                    // node — an unbounded `message_callbacks` leak.
                    let cb = self
                        .message_callbacks
                        .lock()
                        .unwrap()
                        .remove(&packet.reply_to);
                    if let Some(cb) = cb {
                        (cb.fn_)(packet.key.clone(), packet.payload.clone());
                    }
                } else if self.chain_message_targets_local(&packet) {
                    match packet.message_type.as_str() {
                        "vm.cost.negotiate" => {
                            if packet.author == self.id {
                                return String::new();
                            }
                            let cost_per_second =
                                self.cost.lock().unwrap().execution_cost_per_second;
                            let pay = ChainPayPacket {
                                typ: "vm.cost.ack".to_string(),
                                session_id: packet.request_id.clone(),
                                cost_per_second,
                                ..Default::default()
                            };
                            let key = packet.key.clone();
                            let submitter = packet.submitter.clone();
                            let mut receivers: HashMap<String, HashMap<String, bool>> =
                                HashMap::new();
                            receivers.insert(submitter.clone(), HashMap::new());
                            let id = self.id.clone();
                            let signed = self.sign_packet(cost_per_second.to_string().as_bytes());
                            let trans = self.clone();
                            thread::spawn(move || {
                                let reply = ChainMessage {
                                    key,
                                    message_type: "vm.cost.ack".to_string(),
                                    reply_to: packet.request_id.clone(),
                                    recievers: receivers,
                                    signatures: vec![signed],
                                    submitter: id.clone(),
                                    request_id: secure_unique_string(),
                                    author: id,
                                    pay: Some(pay),
                                    ..ChainMessage::default()
                                };
                                trans.submit_chain_op("main", ChainPacketOp::Message(reply));
                            });
                        }
                        "vm.execute.request" | "vm.execute.charge" | "vm.execute" => {
                            if matches!(
                                packet.message_type.as_str(),
                                "vm.execute.request" | "vm.execute.charge"
                            ) {
                                if let Some(pay) = packet.pay.as_ref() {
                                    let free = self
                                        .free_nodes
                                        .lock()
                                        .unwrap()
                                        .contains_key(&packet.submitter);
                                    if !free && !self.consume_pay_lock_on_chain(Some(pay)) {
                                        return String::new();
                                    }
                                    let mut packet_cpy = packet.clone();
                                    let cps = self.cost.lock().unwrap().execution_cost_per_second;
                                    if let Some(p) = packet_cpy.pay.as_mut() {
                                        if p.accepted_seconds <= 0 && cps > 0 {
                                            p.accepted_seconds = p.amount / cps;
                                        }
                                    }
                                    let trans = self.clone();
                                    thread::spawn(move || {
                                        trans.run_chain_message(packet_cpy);
                                    });
                                    return String::new();
                                }
                            }
                            self.run_chain_message(packet);
                        }
                        _ => {}
                    }
                }
                String::new()
            }
            "base" => {
                let packet: ChainBaseRequest = match serde_json::from_slice(trx_payload) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("handle_chain_packet: bad base: {}", e);
                        return String::new();
                    }
                };
                // Only the submitting node holds a callback for its own base
                // request (registered in `send_base_request_on_chain`) and
                // reaps it below once it processes the committed request. On any
                // other node this `or_insert_with` used to park a no-op callback
                // that nothing ever removed — one leaked entry per base request
                // routed through the node, unbounded over its lifetime.
                if packet.submitter == self.id {
                    let mut cbs = self.callbacks.lock().unwrap();
                    cbs.entry(packet.request_id.clone()).or_insert_with(|| {
                        Arc::new(ChainCallback {
                            fn_: Arc::new(|_, _, _| {}),
                            executors: HashMap::new(),
                            responses: HashMap::new(),
                            tag: String::new(),
                        })
                    });
                }
                let user_id = packet
                    .author
                    .strip_prefix("user::")
                    .unwrap_or("")
                    .to_string();
                let secure = match self.actor.fetch_secure_action(&packet.key) {
                    Some(s) => s,
                    None => return String::new(),
                };
                let raw_payload =
                    serde_json::from_slice::<Value>(&packet.payload).unwrap_or(Value::Null);
                let input = match secure.parse_input("chain", raw_payload) {
                    Ok(i) => i,
                    Err(e) => {
                        eprintln!("parse_input chain: {}", e);
                        let signature = self.sign_packet(e.to_string().as_bytes());
                        if let Some(globe) = self.globe.lock().unwrap().clone() {
                            globe.exec_base_response_on_chain(
                                &packet.request_id,
                                Vec::new(),
                                &signature,
                                400,
                                "input parsing error",
                                Vec::new(),
                                &packet.tag,
                                &user_id,
                            );
                        }
                        return String::new();
                    }
                };
                let signature = packet.signatures.get(1).cloned().unwrap_or_default();
                let res = secure.securly_act_chain(
                    &user_id,
                    &packet.request_id,
                    &packet.payload,
                    &signature,
                    input,
                    &packet.submitter,
                    &packet.tag,
                );
                if packet.submitter == self.id {
                    let cb = self.callbacks.lock().unwrap().remove(&packet.request_id);
                    if let Some(cb) = cb {
                        match res {
                            Ok((status, value)) => {
                                let data = serde_json::to_vec(&value).unwrap_or_default();
                                (cb.fn_)(data, status, None);
                            }
                            Err(e) => {
                                (cb.fn_)(b"{}".to_vec(), 500, Some(e));
                            }
                        }
                    }
                }
                String::new()
            }
            _ => String::new(),
        }
    }

    fn submit_chain_op(&self, chain_id: &str, op: ChainPacketOp) {
        if let Some(tx) = self.chain_tx.lock().unwrap().clone() {
            let _ = tx.send(ChainSubmission {
                chain_id: chain_id.to_string(),
                op,
            });
        }
    }
}

impl ICore for Core {
    fn owner_id(&self) -> String {
        self.owner_id.clone()
    }
    fn id(&self) -> String {
        self.id.clone()
    }
    fn gods(&self) -> Vec<String> {
        self.gods.lock().unwrap().clone()
    }
    fn add_god(&self, username: &str) {
        if username.is_empty() {
            return;
        }
        let mut gods = self.gods.lock().unwrap();
        if !gods.iter().any(|g| g == username) {
            gods.push(username.to_string());
        }
    }
    fn tools(&self) -> Arc<dyn ITools> {
        self.tools
            .lock()
            .unwrap()
            .clone()
            .expect("Core.tools accessed before Load()")
    }
    fn free_nodes(&self) -> HashMap<String, bool> {
        self.free_nodes.lock().unwrap().clone()
    }
    fn add_free_node(&self, node_id: &str) {
        if node_id.is_empty() {
            return;
        }
        self.free_nodes
            .lock()
            .unwrap()
            .insert(node_id.to_string(), true);
    }
    fn actor(&self) -> Arc<dyn IActor> {
        self.actor.clone()
    }
    fn load(&self, args: Vec<String>, config: HashMap<String, Value>) {
        // The `args`/`config` here are the same shape as Go's variadic
        // `args ...interface{}` map. We route through `Core::load_inner`
        // which expects strongly-typed paths; this trait method exists so
        // the abstraction signature stays Go-compatible. main.rs uses
        // `load_inner` directly.
        let _ = (args, config);
    }
    fn close(&self) {
        if let Some(tools) = self.tools.lock().unwrap().clone() {
            tools.network().chain().close();
            // The key/value store and the private QuestDB pool close on drop via their Arc owners.
        }
    }
    fn plant_chain_trigger(
        &self,
        count: i64,
        user_id: &str,
        tag: &str,
        machine_id: &str,
        store_id: &str,
        input: &str,
    ) {
        let user_id_owned = user_id.to_string();
        let tag_owned = tag.to_string();
        let machine_id_owned = machine_id.to_string();
        let store_id_owned = store_id.to_string();
        let input_owned = input.to_string();
        self.modify_state(
            false,
            Box::new(move |trx: &dyn ITrx| {
                let tail = secure_unique_string();
                let prefix = format!("chainCallback::{}_{}", user_id_owned, tag_owned);
                let already = !trx.get_by_prefix(&format!("{}|>", prefix)).is_empty();
                trx.put_bytes(&format!("{}|>{}", prefix, tail), vec![0x01]);
                trx.put_bytes(
                    &format!("{}|{}::machineId", prefix, tail),
                    machine_id_owned.as_bytes().to_vec(),
                );
                trx.put_bytes(
                    &format!("{}|{}::storeId", prefix, tail),
                    store_id_owned.as_bytes().to_vec(),
                );
                trx.put_bytes(
                    &format!("{}|{}::attachment", prefix, tail),
                    input_owned.as_bytes().to_vec(),
                );
                if !already {
                    trx.put_bytes(
                        &format!("{}::targetCount", prefix),
                        (count as u32).to_be_bytes().to_vec(),
                    );
                    trx.put_bytes(
                        &format!("{}::tempCount", prefix),
                        0u32.to_be_bytes().to_vec(),
                    );
                }
                Ok(())
            }),
        );
    }
    fn app_pending_trxs(&self) {
        // Grouped chain-transaction execution was a no-op placeholder in the embedded
        // VMM; chain messages reach workloads as invocations (P5-06).
        self.app_pending_trxs.lock().unwrap().clear();
    }
    fn ip_addr(&self) -> String {
        self.ip.clone()
    }
    fn modify_state(&self, readonly: bool, fn_: TrxClosure) {
        if let Some(trx) = self.checked_trx(readonly) {
            run_trx_closure(&trx, fn_);
        }
    }
    fn modify_state_securly_with_source(
        &self,
        readonly: bool,
        info: Arc<dyn IInfo>,
        src: &str,
        fn_: StateClosure,
    ) {
        if let Some(trx) = self.checked_trx(readonly) {
            if let Err(StateFailure::Storage(error)) = run_state_closure(&trx, info, src, fn_) {
                eprintln!("modify_state_securly: {error}");
            }
        }
    }
    fn modify_state_securly(&self, readonly: bool, info: Arc<dyn IInfo>, fn_: StateClosure) {
        self.modify_state_securly_with_source(readonly, info, "", fn_);
    }
    fn modify_state_securly_checked(
        &self,
        readonly: bool,
        info: Arc<dyn IInfo>,
        src: &str,
        fn_: StateClosure,
    ) -> Result<()> {
        let Some(trx) = self.checked_trx(readonly) else {
            return Err(anyhow::anyhow!("state is not available"));
        };
        run_state_closure(&trx, info, src, fn_).map_err(StateFailure::into_error)
    }
    fn sign_packet(&self, data: &[u8]) -> String {
        let key = self.priv_key.lock().unwrap().clone();
        match key {
            Some(k) => Self::sign_with(&k, data),
            None => String::new(),
        }
    }
    fn sign_packet_as_owner(&self, data: &[u8]) -> String {
        Self::sign_with(&self.owner_priv_key, data)
    }
    fn execution_cost_per_second(&self) -> i64 {
        self.cost.lock().unwrap().execution_cost_per_second
    }
    fn vm_ram_cost_per_mb_per_minute(&self) -> i64 {
        self.cost.lock().unwrap().vm_ram_cost_per_mb_minute
    }
    fn vm_cpu_core_cost_per_minute(&self) -> i64 {
        self.cost.lock().unwrap().vm_cpu_core_cost_per_minute
    }
    fn vm_disk_cost_per_gb_per_minute(&self) -> i64 {
        self.cost.lock().unwrap().vm_disk_cost_per_gb_minute
    }
    fn globe(&self) -> Arc<dyn IGlobe> {
        self.globe
            .lock()
            .unwrap()
            .clone()
            .expect("Core.globe accessed before Load()")
    }
}

use crate::shell::api::model::core_storage::{run_action, StateFailure};

/// Run a transaction closure with ADR 0026 commit ordering; a storage failure is
/// logged (LD-10), an action failure is the closure's own answer.
fn run_trx_closure(trx: &Arc<TrxWrapper>, mut fn_: TrxClosure) {
    if let Err(StateFailure::Storage(error)) =
        run_action(|| fn_(&**trx), || trx.commit(), || trx.discard())
    {
        eprintln!("modify_state: {error}");
    }
}

/// Run a secured state closure with ADR 0026 commit ordering.
fn run_state_closure(
    trx: &Arc<TrxWrapper>,
    info: Arc<dyn IInfo>,
    src: &str,
    mut fn_: StateClosure,
) -> Result<(), StateFailure> {
    let state: Arc<dyn crate::models::state::IState> =
        Arc::new(ActorState::new(Some(info), Some(trx.clone()), src));
    run_action(|| fn_(state), || trx.commit(), || trx.discard())
}

impl Core {
    /// A transaction over this core's storage, when the tools are loaded.
    fn checked_trx(&self, readonly: bool) -> Option<Arc<TrxWrapper>> {
        let tools = self.tools.lock().unwrap().clone()?;
        Some(TrxWrapper::new(self.weak_self(), tools.storage(), readonly))
    }

    /// Build a fresh `Arc<dyn ICore>` pointing at the same underlying
    /// `Core` state. Used by paths that need to hand an `Arc<dyn ICore>`
    /// to drivers / closures.
    fn weak_self(&self) -> Arc<dyn ICore> {
        // We can't recover the real `Arc<Core>` from `&self` without an
        // upgrade target, so construct a forwarding wrapper that holds
        // references to every interior field. For our use sites the
        // wrapper is short-lived (one transaction), so the extra Arc
        // allocations are not a hot path.
        Arc::new(WeakCoreView {
            inner: CoreWeakHandles {
                tools: self.tools.lock().unwrap().clone(),
                actor: self.actor.clone(),
                owner_id: self.owner_id.clone(),
                id: self.id.clone(),
                ip: self.ip.clone(),
                owner_priv_key: self.owner_priv_key.clone(),
                priv_key: self.priv_key.lock().unwrap().clone(),
                cost: self.cost.lock().unwrap().clone(),
                globe: self.globe.lock().unwrap().clone(),
                gods: self.gods.lock().unwrap().clone(),
                free_nodes: self.free_nodes.lock().unwrap().clone(),
            },
        })
    }

    /// Runtime start phase invoked after load/module initialization.
    pub fn run(self: &Arc<Self>) {}

    /// Strongly-typed `Load`. Run once on startup after the constructor.
    pub fn load_inner(
        self: &Arc<Self>,
        gods: Vec<String>,
        storage_root: &str,
        base_db_path: &str,
        applet_db_path: &str,
        store_logs_db: &str,
        searcher_db: &str,
    ) -> Result<()> {
        *self.gods.lock().unwrap() = gods;
        let _ = applet_db_path; // currently fed straight into Vmm
        let _ = store_logs_db;
        let _ = searcher_db;

        // Stage 1 of federation must run before the rest so we can pass
        // the same `Arc<FedNet>` into the storage / network drivers.
        let fed: Arc<FedNet> = FedNet::first_stage(self.clone());
        let storage: Arc<dyn IStorage> = Storage::new(
            self.clone(),
            storage_root,
            base_db_path,
            store_logs_db,
            searcher_db,
            self.config
                .as_ref()
                .map(|config| config.legacy_adapters.questdb_port)
                .unwrap_or(8812),
        )?;
        let signaler: Arc<dyn ISignaler> = Signaler::new(self.clone(), fed.clone());
        let security: Arc<dyn ISecurity> = Security::new(self.clone(), storage_root);
        let chain: Arc<dyn crate::models::ports::network::chain::IChain> =
            Blockchain::new(self.clone(), storage_root);
        let tls_cfg = match self.config.as_ref().map(|config| &config.core) {
            Some(config) => match (&config.tls_certificate_path, &config.tls_private_key_path) {
                (Some(cert), Some(key)) => match tls_config_from_files(&cert, &key) {
                    Ok(cfg) => Some(cfg),
                    Err(e) => {
                        eprintln!("TLS config load failed: {}; running without TLS", e);
                        None
                    }
                },
                _ => None,
            },
            None => None,
        };
        let network: Arc<dyn INetwork> = NetworkDriver::new(
            self.clone(),
            storage.clone(),
            security.clone(),
            signaler.clone(),
            fed.clone(),
            chain.clone(),
            tls_cfg,
        );
        let vmm: Arc<dyn IWorkloads> = NodeWorkloads::new(self.clone());

        // Stage 2 — federation needs storage/signaler.
        fed.second_stage(storage.clone(), signaler.clone());

        // Load the server private key for signing.
        let pem = security.fetch_key_pair("server_key");
        if let Some(first) = pem.into_iter().next() {
            if let Ok(key) = Self::parse_private_key(&first) {
                *self.priv_key.lock().unwrap() = Some(Arc::new(key));
            }
        }

        // Cross-protocol client-request rate limiter. One instance is shared by
        // every client-facing transport (TCP / WS / HTTP ingress) so a client's
        // quota is unified across protocols.
        let rate_limiter: Arc<dyn crate::models::ports::ratelimit::IRateLimiter> = match self
            .config
            .as_ref()
        {
            Some(config) => crate::drivers::ratelimit::RateLimiter::from_typed(&config.rate_limit),
            None => crate::drivers::ratelimit::RateLimiter::new(Default::default()),
        };

        // Install tools + chain restore.
        let tools: Arc<dyn ITools> = Arc::new(Tools {
            security,
            signaler,
            storage,
            network: network.clone(),
            vmm,
            rate_limiter,
        });
        *self.tools.lock().unwrap() = Some(tools);
        network.chain().restore_from_storage();

        // Cost knobs are validated once by the composition root.
        let mut cost = self.cost.lock().unwrap();
        if let Some(config) = self.config.as_ref().map(|config| &config.core) {
            cost.execution_cost_per_second = config.execution_cost_per_second;
            cost.vm_ram_cost_per_mb_minute = config.ram_cost_per_mb_minute;
            cost.vm_cpu_core_cost_per_minute = config.cpu_core_cost_per_minute;
            cost.vm_disk_cost_per_gb_minute = config.disk_cost_per_gb_minute;
        }
        drop(cost);

        // Chain submission channel.
        let (chain_tx, chain_rx) = crossbeam_channel::unbounded::<ChainSubmission>();
        *self.chain_tx.lock().unwrap() = Some(chain_tx.clone());

        // Globe.
        let peers_fn = {
            let net = network.clone();
            Arc::new(move || net.chain().peers()) as crate::core::globe::PeersFn
        };
        let sign_fn: crate::core::globe::SignPacketFn = {
            let me = self.clone();
            Arc::new(move |data| me.sign_packet(data))
        };
        let submit_fn: crate::core::globe::SubmitChainPacketFn = {
            let chain_tx_clone = chain_tx.clone();
            Arc::new(move |chain_id: &str, op: ChainPacketOp| {
                let _ = chain_tx_clone.send(ChainSubmission {
                    chain_id: chain_id.to_string(),
                    op,
                });
            })
        };
        let set_chain_callback_fn: crate::core::globe::SetChainCallbackFn = {
            let core_for_cb = self.clone();
            Arc::new(move |callback_id: &str, cb: ChainCallback| {
                core_for_cb
                    .callbacks
                    .lock()
                    .unwrap()
                    .insert(callback_id.to_string(), Arc::new(cb));
            })
        };
        let set_message_cb_fn: crate::core::globe::SetMessageCbFn = {
            let core_for_msg = self.clone();
            Arc::new(move |callback_id: &str, cb: MessageCallback| {
                core_for_msg
                    .message_callbacks
                    .lock()
                    .unwrap()
                    .insert(callback_id.to_string(), Arc::new(cb));
            })
        };
        let globe = Globe::new(
            self.id.clone(),
            self.ip.clone(),
            peers_fn,
            sign_fn,
            submit_fn,
            set_chain_callback_fn,
            set_message_cb_fn,
            MAX_VALIDATOR_COUNT,
            ELECTION_COMMIT_SECONDS,
            ELECTION_REVEAL_SECONDS,
        );
        *self.globe.lock().unwrap() = Some(globe.clone());

        // Wire the chain pipeline so committed blocks flow through
        // `handle_chain_packet`.
        let trans = self.clone();
        let pipeline: crate::models::ports::network::chain::PipelineFn = Box::new(
            move |txs: Vec<Vec<u8>>, insider_cb: Box<dyn Fn(Vec<u8>) + Send + Sync>| {
                let mut machine_ids: Vec<String> = Vec::new();
                for tx in txs {
                    let s = String::from_utf8_lossy(&tx);
                    let first_index = match s.find("::") {
                        Some(i) => i,
                        None => continue,
                    };
                    let typ = &s[..first_index];
                    let body = &tx[first_index + 2..];
                    if typ == "nodeJoined" {
                        insider_cb(tx.clone());
                    } else if typ == &format!("sharderMap|{}", trans.id) {
                        insider_cb(tx.clone());
                    } else {
                        let r = trans.handle_chain_packet(typ, body);
                        if !r.is_empty() {
                            machine_ids.push(r);
                        }
                    }
                }
                trans.app_pending_trxs();
                machine_ids
            },
        );
        network.chain().register_pipeline(pipeline);

        // Background: drain the chain submission queue and forward each
        // payload onto the right shard.
        let trans = self.clone();
        thread::spawn(move || {
            while let Ok(envelope) = chain_rx.recv() {
                let chain_id = if envelope.chain_id.is_empty() {
                    "main".to_string()
                } else {
                    envelope.chain_id.clone()
                };
                let (typ, payload) = match &envelope.op {
                    ChainPacketOp::BaseRequest(req) => (
                        "base".to_string(),
                        serde_json::to_vec(req).unwrap_or_default(),
                    ),
                    ChainPacketOp::Message(m) => (
                        "message".to_string(),
                        serde_json::to_vec(m).unwrap_or_default(),
                    ),
                    ChainPacketOp::Election(e) => (
                        "election".to_string(),
                        serde_json::to_vec(e).unwrap_or_default(),
                    ),
                    ChainPacketOp::Stake(s) => (
                        "stake".to_string(),
                        serde_json::to_vec(s).unwrap_or_default(),
                    ),
                    ChainPacketOp::Response(r) => (
                        "response".to_string(),
                        serde_json::to_vec(r).unwrap_or_default(),
                    ),
                };
                let machine_id = match &envelope.op {
                    ChainPacketOp::Message(m) => trans
                        .chain_message_machine_ids(m)
                        .into_keys()
                        .next()
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                let mut framed = Vec::new();
                framed.extend_from_slice(typ.as_bytes());
                framed.extend_from_slice(b"::");
                framed.extend_from_slice(&payload);
                trans
                    .tools()
                    .network()
                    .chain()
                    .submit_trx(&chain_id, &machine_id, &typ, framed);
            }
        });

        // Background: every second, ask the globe to start a scheduled
        // election if the hour aligns.
        let trans = self.clone();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(1));
            let globe = trans.globe.lock().unwrap().clone();
            if let Some(g) = globe {
                g.try_start_scheduled_election(SystemTime::now());
            }
        });

        Ok(())
    }
}

/// Small forwarding shim around the parts of `Core` that an `Arc<dyn ICore>`
/// needs. `Core::weak_self` builds one of these on demand so the
/// `modify_state` family can synthesise an `Arc<dyn ICore>` for `TrxWrapper`
/// without holding a real reference to itself.
struct CoreWeakHandles {
    tools: Option<Arc<dyn ITools>>,
    actor: Arc<dyn IActor>,
    owner_id: String,
    id: String,
    ip: String,
    owner_priv_key: Arc<RsaPrivateKey>,
    priv_key: Option<Arc<RsaPrivateKey>>,
    cost: CostConfig,
    globe: Option<Arc<dyn IGlobe>>,
    gods: Vec<String>,
    free_nodes: HashMap<String, bool>,
}

struct WeakCoreView {
    inner: CoreWeakHandles,
}

impl WeakCoreView {
    /// A transaction over this view's storage, when the tools are loaded.
    fn checked_trx(&self, readonly: bool) -> Option<Arc<TrxWrapper>> {
        let tools = self.inner.tools.clone()?;
        let core_for_trx: Arc<dyn ICore> = Arc::new(WeakCoreView {
            inner: CoreWeakHandles {
                ..clone_handles(&self.inner)
            },
        });
        Some(TrxWrapper::new(core_for_trx, tools.storage(), readonly))
    }
}

impl ICore for WeakCoreView {
    fn owner_id(&self) -> String {
        self.inner.owner_id.clone()
    }
    fn id(&self) -> String {
        self.inner.id.clone()
    }
    fn gods(&self) -> Vec<String> {
        self.inner.gods.clone()
    }
    fn add_god(&self, _: &str) {}
    fn tools(&self) -> Arc<dyn ITools> {
        self.inner.tools.clone().expect("tools unset on weak view")
    }
    fn free_nodes(&self) -> HashMap<String, bool> {
        self.inner.free_nodes.clone()
    }
    fn add_free_node(&self, _: &str) {}
    fn actor(&self) -> Arc<dyn IActor> {
        self.inner.actor.clone()
    }
    fn load(&self, _: Vec<String>, _: HashMap<String, Value>) {}
    fn close(&self) {}
    fn plant_chain_trigger(&self, _: i64, _: &str, _: &str, _: &str, _: &str, _: &str) {}
    fn app_pending_trxs(&self) {}
    fn ip_addr(&self) -> String {
        self.inner.ip.clone()
    }
    fn modify_state(&self, readonly: bool, fn_: TrxClosure) {
        if let Some(trx) = self.checked_trx(readonly) {
            run_trx_closure(&trx, fn_);
        }
    }
    fn modify_state_securly_with_source(
        &self,
        readonly: bool,
        info: Arc<dyn IInfo>,
        src: &str,
        fn_: StateClosure,
    ) {
        if let Some(trx) = self.checked_trx(readonly) {
            if let Err(StateFailure::Storage(error)) = run_state_closure(&trx, info, src, fn_) {
                eprintln!("modify_state_securly: {error}");
            }
        }
    }
    fn modify_state_securly(&self, readonly: bool, info: Arc<dyn IInfo>, fn_: StateClosure) {
        self.modify_state_securly_with_source(readonly, info, "", fn_);
    }
    fn modify_state_securly_checked(
        &self,
        readonly: bool,
        info: Arc<dyn IInfo>,
        src: &str,
        fn_: StateClosure,
    ) -> Result<()> {
        let Some(trx) = self.checked_trx(readonly) else {
            return Err(anyhow::anyhow!("state is not available"));
        };
        run_state_closure(&trx, info, src, fn_).map_err(StateFailure::into_error)
    }
    fn sign_packet(&self, data: &[u8]) -> String {
        match &self.inner.priv_key {
            Some(k) => Core::sign_with(k, data),
            None => String::new(),
        }
    }
    fn sign_packet_as_owner(&self, data: &[u8]) -> String {
        Core::sign_with(&self.inner.owner_priv_key, data)
    }
    fn execution_cost_per_second(&self) -> i64 {
        self.inner.cost.execution_cost_per_second
    }
    fn vm_ram_cost_per_mb_per_minute(&self) -> i64 {
        self.inner.cost.vm_ram_cost_per_mb_minute
    }
    fn vm_cpu_core_cost_per_minute(&self) -> i64 {
        self.inner.cost.vm_cpu_core_cost_per_minute
    }
    fn vm_disk_cost_per_gb_per_minute(&self) -> i64 {
        self.inner.cost.vm_disk_cost_per_gb_minute
    }
    fn globe(&self) -> Arc<dyn IGlobe> {
        self.inner.globe.clone().expect("Globe unset on weak view")
    }
}

fn clone_handles(h: &CoreWeakHandles) -> CoreWeakHandles {
    CoreWeakHandles {
        tools: h.tools.clone(),
        actor: h.actor.clone(),
        owner_id: h.owner_id.clone(),
        id: h.id.clone(),
        ip: h.ip.clone(),
        owner_priv_key: h.owner_priv_key.clone(),
        priv_key: h.priv_key.clone(),
        cost: h.cost.clone(),
        globe: h.globe.clone(),
        gods: h.gods.clone(),
        free_nodes: h.free_nodes.clone(),
    }
}

#[allow(dead_code)]
fn _force_use() -> Result<()> {
    Ok(())
}

// Pull in BaseInfo / json to keep imports tight.
#[allow(dead_code)]
fn _hint(_: BaseInfo, _: Value) {}
