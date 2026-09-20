//! Translation of `core/module/globe/globe.go`.
//!
//! `Globe` implements the [`IGlobe`] trait: chain request/response plumbing,
//! validator-set staking, and the hourly weighted-validator election round.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::models::chain::{
    ChainBaseRequest, ChainCallback, ChainElectionPacket, ChainMessage, ChainPayPacket,
    ChainResponse, ChainStakePacket, Effects, ElectionRound, MessageCallback, StakingNodeState,
    ValidatorSetRecord,
};
use crate::models::globe::{BaseResponseCallback, IGlobe, TypedMessageCallback};
use crate::models::update::Update;
use crate::shell::utils::crypto::secure_unique_string;
use crate::util::AnyVal;

pub const STAKE_ACTION_BOND: &str = "bond";
pub const STAKE_ACTION_UNBOND: &str = "unbond";
pub const STAKE_ACTION_SLASH: &str = "slash";
pub const MIN_VALIDATOR_STAKE: i64 = 1_000;
pub const MAX_VALIDATOR_STAKE: i64 = 1_000_000_000_000;
pub const UNBONDING_SECONDS: i64 = 24 * 60 * 60;

/// Signature for `signPacketFn` injected at construction time.
pub type SignPacketFn = Arc<dyn Fn(&[u8]) -> String + Send + Sync>;

/// Signature for `peersFn`.
pub type PeersFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Signature for `submitChainPacketFn` — payload boxed as `AnyVal` so the
/// caller can shuttle any of the chain packet variants.
pub type SubmitChainPacketFn = Arc<dyn Fn(&str, ChainPacketOp) + Send + Sync>;

/// Closure stash for chain base callbacks.
pub type SetChainCallbackFn = Arc<dyn Fn(&str, ChainCallback) + Send + Sync>;

/// Closure stash for typed-message callbacks.
pub type SetMessageCbFn = Arc<dyn Fn(&str, MessageCallback) + Send + Sync>;

/// All packet variants the submit-chain function can carry. Mirrors the Go
/// switch in `core.go::handleChainPacket`.
#[derive(Debug, Clone)]
pub enum ChainPacketOp {
    BaseRequest(ChainBaseRequest),
    Message(ChainMessage),
    Response(ChainResponse),
    Election(ChainElectionPacket),
    Stake(ChainStakePacket),
}

/// `Globe` is the validator-set + chain-RPC coordinator.
pub struct Globe {
    node_id: String,
    voter_id: String,
    max_validator_count: usize,
    election_commit_secs: i64,
    election_reveal_secs: i64,
    peers_fn: PeersFn,
    sign_packet_fn: SignPacketFn,
    submit_chain_packet_fn: SubmitChainPacketFn,
    set_chain_callback_fn: SetChainCallbackFn,
    set_message_cb_fn: SetMessageCbFn,

    inner: Mutex<Inner>,
}

struct Inner {
    node_stakes: HashMap<String, i64>,
    node_owners: HashMap<String, String>,
    node_states: HashMap<String, StakingNodeState>,
    election_round: Option<ElectionRound>,
    last_election_hour: String,
    total_bonded_stake: i64,
    validator_history: HashMap<String, ValidatorSetRecord>,
}

impl Globe {
    /// Mirrors `NewGlobe(...)`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_id: String,
        voter_id: String,
        peers_fn: PeersFn,
        sign_packet_fn: SignPacketFn,
        submit_chain_packet_fn: SubmitChainPacketFn,
        set_chain_callback_fn: SetChainCallbackFn,
        set_message_cb_fn: SetMessageCbFn,
        max_validator_count: usize,
        election_commit_secs: i64,
        election_reveal_secs: i64,
    ) -> Arc<Globe> {
        Arc::new(Globe {
            node_id,
            voter_id,
            max_validator_count,
            election_commit_secs,
            election_reveal_secs,
            peers_fn,
            sign_packet_fn,
            submit_chain_packet_fn,
            set_chain_callback_fn,
            set_message_cb_fn,
            inner: Mutex::new(Inner {
                node_stakes: HashMap::new(),
                node_owners: HashMap::new(),
                node_states: HashMap::new(),
                election_round: None,
                last_election_hour: String::new(),
                total_bonded_stake: 0,
                validator_history: HashMap::new(),
            }),
        })
    }
}

impl IGlobe for Globe {
    fn send_base_request_on_chain(
        &self,
        key: &str,
        payload: Vec<u8>,
        signature: &str,
        user_id: &str,
        tag: &str,
        callback: BaseResponseCallback,
    ) {
        let callback_id = secure_unique_string();
        // `BaseResponseCallback` is `Box<dyn Fn>`; ChainCallback wants
        // `Arc<dyn Fn>`. Re-wrap on the way through.
        let cb_arc: Arc<dyn Fn(Vec<u8>, i64, Option<crate::util::GoError>) + Send + Sync> =
            Arc::from(callback);
        (self.set_chain_callback_fn)(
            &callback_id,
            ChainCallback {
                fn_: cb_arc,
                executors: HashMap::new(),
                responses: HashMap::new(),
                tag: tag.to_string(),
            },
        );
        let req = ChainBaseRequest {
            key: key.to_string(),
            author: format!("user::{}", user_id),
            submitter: self.node_id.clone(),
            payload: payload.clone(),
            signatures: vec![(self.sign_packet_fn)(&payload), signature.to_string()],
            request_id: callback_id,
            tag: tag.to_string(),
        };
        (self.submit_chain_packet_fn)("main", ChainPacketOp::BaseRequest(req));
    }

    #[allow(clippy::too_many_arguments)]
    fn send_typed_message_on_chain(
        &self,
        chain_id: &str,
        key: &str,
        message_type: &str,
        payload: Vec<u8>,
        signature: &str,
        user_id: &str,
        receivers: HashMap<String, HashMap<String, bool>>,
        reply_to: &str,
        store_id: &str,
        pay: Option<ChainPayPacket>,
        callback: Option<TypedMessageCallback>,
    ) {
        let callback_id = secure_unique_string();
        // Register a reply callback only when the caller actually wants one.
        // Fire-and-forget sends pass `None`, so no entry is parked in
        // `message_callbacks` — previously every send left a permanent entry
        // there (it was never removed on delivery either), an unbounded leak on
        // the on-chain messaging path.
        if let Some(callback) = callback {
            let cb_arc: Arc<dyn Fn(String, Vec<u8>) + Send + Sync> = Arc::from(callback);
            (self.set_message_cb_fn)(
                &callback_id,
                MessageCallback {
                    id: callback_id.clone(),
                    fn_: cb_arc,
                },
            );
        }
        let chain_id = if chain_id.is_empty() {
            "main"
        } else {
            chain_id
        };
        let msg = ChainMessage {
            key: key.to_string(),
            message_type: message_type.to_string(),
            author: format!("user::{}", user_id),
            submitter: self.node_id.clone(),
            payload: payload.clone(),
            signatures: vec![(self.sign_packet_fn)(&payload), signature.to_string()],
            request_id: callback_id,
            recievers: receivers,
            reply_to: reply_to.to_string(),
            store_id: store_id.to_string(),
            pay,
        };
        (self.submit_chain_packet_fn)(chain_id, ChainPacketOp::Message(msg));
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_base_response_on_chain(
        &self,
        callback_id: &str,
        packet: Vec<u8>,
        signature: &str,
        res_code: i64,
        e: &str,
        mut updates: Vec<Update>,
        tag: &str,
        to_user_id: &str,
    ) {
        updates.sort_by(|a, b| {
            let ka = format!("{}:{}", a.typ, a.key);
            let kb = format!("{}:{}", b.typ, b.key);
            ka.cmp(&kb)
        });
        let resp = ChainResponse {
            to_user_id: to_user_id.to_string(),
            tag: tag.to_string(),
            signature: signature.to_string(),
            executor: self.node_id.clone(),
            request_id: callback_id.to_string(),
            res_code,
            err: e.to_string(),
            payload: packet,
            effects: Effects {
                db_updates: updates,
            },
        };
        (self.submit_chain_packet_fn)("main", ChainPacketOp::Response(resp));
    }

    fn handle(&self, typ: &str, trx_payload: Vec<u8>) -> bool {
        match typ {
            "stake" => self.handle_stake_packet_payload(&trx_payload),
            "election" => self.handle_election_packet_payload(&trx_payload),
            _ => false,
        }
    }

    fn stake_node_owner(&self, node_id: &str, owner_id: &str, amount: i64) {
        if node_id.is_empty() || owner_id.is_empty() || amount < 0 {
            return;
        }
        let next_nonce = {
            let inner = self.inner.lock().unwrap();
            inner
                .node_states
                .get(node_id)
                .map(|s| s.nonce + 1)
                .unwrap_or(1)
        };
        let pkt = ChainStakePacket {
            node_id: node_id.to_string(),
            owner_id: owner_id.to_string(),
            action: STAKE_ACTION_BOND.to_string(),
            amount,
            nonce: next_nonce,
            lock_seconds: 0,
            reason: String::new(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
        };
        (self.submit_chain_packet_fn)("main", ChainPacketOp::Stake(pkt));
    }

    fn try_start_scheduled_election(&self, now: SystemTime) {
        let dt: DateTime<Utc> = now.into();
        let hour_key = format!(
            "{:04}-{:02}-{:02}T{:02}",
            dt.year(),
            dt.month(),
            dt.day(),
            dt.hour()
        );

        {
            let mut inner = self.inner.lock().unwrap();
            if inner.last_election_hour == hour_key {
                return;
            }
            if dt.minute() != 0 || dt.second() > 2 {
                return;
            }
            inner.last_election_hour = hour_key.clone();
        }

        let round_id = hour_key;
        let commit_seed = secure_unique_string();
        self.submit_election_packet(
            election_meta(&[
                ("phase", "start-round"),
                ("roundId", &round_id),
                ("voter", &self.voter_id),
                ("commitSeed", &commit_seed),
            ]),
            b"{}".to_vec(),
        );

        let submit = self.submit_chain_packet_fn.clone();
        let commit_secs = self.election_commit_secs;
        let reveal_secs = self.election_reveal_secs;
        let round_for_thread = round_id.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(commit_secs.max(0) as u64));
            submit(
                "main",
                ChainPacketOp::Election(ChainElectionPacket {
                    typ: "election".to_string(),
                    key: "choose-validator".to_string(),
                    meta: election_meta(&[
                        ("phase", "start-reveal"),
                        ("roundId", &round_for_thread),
                    ]),
                    payload: b"{}".to_vec(),
                }),
            );
            thread::sleep(Duration::from_secs(reveal_secs.max(0) as u64));
            submit(
                "main",
                ChainPacketOp::Election(ChainElectionPacket {
                    typ: "election".to_string(),
                    key: "choose-validator".to_string(),
                    meta: election_meta(&[("phase", "finalize"), ("roundId", &round_for_thread)]),
                    payload: b"{}".to_vec(),
                }),
            );
        });
    }
}

impl Globe {
    fn submit_election_packet(&self, meta: HashMap<String, Value>, payload: Vec<u8>) {
        let pkt = ChainElectionPacket {
            typ: "election".to_string(),
            key: "choose-validator".to_string(),
            meta,
            payload: if payload.is_empty() {
                b"{}".to_vec()
            } else {
                payload
            },
        };
        (self.submit_chain_packet_fn)("main", ChainPacketOp::Election(pkt));
    }

    fn handle_stake_packet_payload(&self, payload: &[u8]) -> bool {
        let pkt: ChainStakePacket = match serde_json::from_slice(payload) {
            Ok(p) => p,
            Err(_) => return true,
        };
        if pkt.node_id.is_empty() || pkt.owner_id.is_empty() || pkt.amount <= 0 {
            return true;
        }
        let action = if pkt.action.is_empty() {
            STAKE_ACTION_BOND.to_string()
        } else {
            pkt.action.clone()
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let mut inner = self.inner.lock().unwrap();
        self.settle_mature_unbonds_locked(&mut inner, now);

        let state = inner
            .node_states
            .entry(pkt.node_id.clone())
            .or_insert_with(|| StakingNodeState {
                node_id: pkt.node_id.clone(),
                owner_id: pkt.owner_id.clone(),
                ..StakingNodeState::default()
            });
        if !state.owner_id.is_empty() && state.owner_id != pkt.owner_id {
            return true;
        }
        if pkt.nonce > 0 && pkt.nonce <= state.nonce {
            return true;
        }
        if pkt.nonce > 0 {
            state.nonce = pkt.nonce;
        }
        state.owner_id = pkt.owner_id.clone();

        let owner = state.owner_id.clone();
        let bonded;
        match action.as_str() {
            "bond" => Self::apply_bond(state, pkt.amount, pkt.lock_seconds, now, &mut 0),
            "unbond" => Self::apply_unbond(state, pkt.amount, now, &mut 0),
            "slash" => Self::apply_slash(state, pkt.amount, now, &mut 0),
            _ => return true,
        }
        state.last_updated_at = now;
        bonded = state.bonded_stake;

        // The mutation paths above shifted `total_bonded_stake` via a local
        // mutable scratch; re-derive it from the state map for the borrow
        // checker's sake (rare path, cheap).
        let total: i64 = inner.node_states.values().map(|s| s.bonded_stake).sum();
        inner.total_bonded_stake = total;
        inner.node_owners.insert(pkt.node_id.clone(), owner);
        inner.node_stakes.insert(pkt.node_id.clone(), bonded);
        true
    }

    fn apply_bond(
        state: &mut StakingNodeState,
        amount: i64,
        lock_seconds: i64,
        now: i64,
        _: &mut i64,
    ) {
        if amount <= 0 {
            return;
        }
        let mut new_bonded = state.bonded_stake + amount;
        if new_bonded > MAX_VALIDATOR_STAKE {
            new_bonded = MAX_VALIDATOR_STAKE;
        }
        state.bonded_stake = new_bonded;
        if lock_seconds > 0 {
            let lock_until = now + lock_seconds;
            if lock_until > state.unbond_unlock_at {
                state.unbond_unlock_at = lock_until;
            }
        }
    }

    fn apply_unbond(state: &mut StakingNodeState, amount: i64, now: i64, _: &mut i64) {
        if amount <= 0 || state.bonded_stake <= 0 || now < state.unbond_unlock_at {
            return;
        }
        let amount = amount.min(state.bonded_stake);
        state.bonded_stake -= amount;
        state.pending_unbond += amount;
        state.unbond_unlock_at = now + UNBONDING_SECONDS;
    }

    fn apply_slash(state: &mut StakingNodeState, amount: i64, now: i64, _: &mut i64) {
        if amount <= 0 {
            return;
        }
        let slash_bonded = amount.min(state.bonded_stake);
        state.bonded_stake -= slash_bonded;
        let mut remaining = amount - slash_bonded;
        if remaining > 0 {
            remaining = remaining.min(state.pending_unbond);
            state.pending_unbond -= remaining;
        }
        state.unbond_unlock_at = now + UNBONDING_SECONDS;
    }

    fn settle_mature_unbonds_locked(&self, inner: &mut Inner, now: i64) {
        for state in inner.node_states.values_mut() {
            if state.pending_unbond > 0 && now >= state.unbond_unlock_at {
                state.pending_unbond = 0;
            }
        }
    }

    fn handle_election_packet_payload(&self, payload: &[u8]) -> bool {
        let pkt: ChainElectionPacket = match serde_json::from_slice(payload) {
            Ok(p) => p,
            Err(_) => return true,
        };
        self.handle_election_packet(pkt);
        true
    }

    fn election_commit(&self, round_id: &str, seed: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(format!("{}::{}::{}", round_id, self.node_id, seed));
        hex::encode(hasher.finalize())
    }

    fn weighted_validators_from_seed(&self, seed: &str, inner: &Inner) -> Vec<String> {
        let peers = (self.peers_fn)();
        if peers.is_empty() {
            return Vec::new();
        }
        let mut candidates: Vec<(String, i64, u64)> = Vec::new();
        for node_id in &peers {
            let state = match inner.node_states.get(node_id) {
                Some(s) if s.bonded_stake >= MIN_VALIDATOR_STAKE => s,
                _ => continue,
            };
            let weight = state.bonded_stake as u64;
            let mut hasher = Sha256::new();
            hasher.update(format!("{}::{}", seed, node_id));
            let hashed = hasher.finalize();
            let raw_score = u64::from_be_bytes(hashed[..8].try_into().unwrap());
            let score = raw_score / weight.max(1);
            candidates.push((node_id.clone(), state.bonded_stake, score));
        }
        candidates.sort_by(|a, b| match a.2.cmp(&b.2) {
            std::cmp::Ordering::Equal => b.1.cmp(&a.1),
            other => other,
        });
        let mut target = (candidates.len() / 3).max(1).min(self.max_validator_count);
        target = target.min(candidates.len());
        candidates
            .into_iter()
            .take(target)
            .map(|(id, _, _)| id)
            .collect()
    }

    fn handle_election_packet(&self, packet: ChainElectionPacket) {
        let phase = match packet.meta.get("phase").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => return,
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let mut inner = self.inner.lock().unwrap();
        self.settle_mature_unbonds_locked(&mut inner, now);

        match phase.as_str() {
            "start-round" => {
                let round_id = packet
                    .meta
                    .get("roundId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let commit_seed = packet
                    .meta
                    .get("commitSeed")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if round_id.is_empty() {
                    return;
                }
                let my_seed = secure_unique_string();
                let commit = self.election_commit(round_id, &my_seed);
                let mut round = ElectionRound {
                    id: round_id.to_string(),
                    commit_seed: commit_seed.to_string(),
                    phase: "commit".to_string(),
                    start_at: now,
                    commit_deadline: now + self.election_commit_secs,
                    reveal_deadline: now + self.election_commit_secs + self.election_reveal_secs,
                    finalized_at: 0,
                    commits: HashMap::new(),
                    reveals: HashMap::new(),
                    commit_participants: HashMap::new(),
                    selected_validators: Vec::new(),
                };
                round.commits.insert(self.node_id.clone(), commit.clone());
                round.reveals.insert(self.node_id.clone(), my_seed);
                round.commit_participants.insert(self.node_id.clone(), true);
                inner.election_round = Some(round);
                drop(inner);
                self.submit_election_packet(
                    election_meta(&[
                        ("phase", "commit"),
                        ("roundId", round_id),
                        ("nodeId", &self.node_id),
                        ("commit", &commit),
                    ]),
                    b"{}".to_vec(),
                );
            }
            "commit" => {
                let round = match inner.election_round.as_mut() {
                    Some(r) => r,
                    None => return,
                };
                let round_id = packet
                    .meta
                    .get("roundId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let node_id = packet
                    .meta
                    .get("nodeId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let commit = packet
                    .meta
                    .get("commit")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if round_id != round.id || node_id.is_empty() || commit.is_empty() {
                    return;
                }
                round
                    .commits
                    .insert(node_id.to_string(), commit.to_string());
                round.commit_participants.insert(node_id.to_string(), true);
            }
            "start-reveal" => {
                let (round_id, my_seed) = {
                    let round = match inner.election_round.as_mut() {
                        Some(r) => r,
                        None => return,
                    };
                    let round_id = packet
                        .meta
                        .get("roundId")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if round_id != round.id {
                        return;
                    }
                    round.phase = "reveal".to_string();
                    (
                        round.id.clone(),
                        round
                            .reveals
                            .get(&self.node_id)
                            .cloned()
                            .unwrap_or_default(),
                    )
                };
                drop(inner);
                if !my_seed.is_empty() {
                    self.submit_election_packet(
                        election_meta(&[
                            ("phase", "reveal"),
                            ("roundId", &round_id),
                            ("nodeId", &self.node_id),
                            ("seed", &my_seed),
                        ]),
                        b"{}".to_vec(),
                    );
                }
            }
            "reveal" => {
                let round = match inner.election_round.as_mut() {
                    Some(r) => r,
                    None => return,
                };
                let round_id = packet
                    .meta
                    .get("roundId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let node_id = packet
                    .meta
                    .get("nodeId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let seed = packet
                    .meta
                    .get("seed")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if round_id != round.id || node_id.is_empty() || seed.is_empty() {
                    return;
                }
                let expected = match round.commits.get(node_id).cloned() {
                    Some(c) if !c.is_empty() => c,
                    _ => return,
                };
                let computed = {
                    let mut hasher = Sha256::new();
                    hasher.update(format!("{}::{}::{}", round_id, node_id, seed));
                    hex::encode(hasher.finalize())
                };
                if computed != expected {
                    return;
                }
                round.reveals.insert(node_id.to_string(), seed.to_string());
            }
            "finalize" => {
                let total = inner.total_bonded_stake;
                let (round_id_owned, global_seed) = {
                    let round = match inner.election_round.as_ref() {
                        Some(r) => r,
                        None => return,
                    };
                    let round_id = packet
                        .meta
                        .get("roundId")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if round_id != round.id {
                        return;
                    }
                    let mut reveal_parts: Vec<String> = round
                        .reveals
                        .iter()
                        .map(|(n, s)| format!("{}={}", n, s))
                        .collect();
                    reveal_parts.sort();
                    let seed = format!("{}::{}", round.id, reveal_parts.join("|"));
                    (round.id.clone(), seed)
                };
                let validators = self.weighted_validators_from_seed(&global_seed, &inner);
                let round = inner.election_round.as_mut().unwrap();
                round.selected_validators = validators.clone();
                round.phase = "finalized".to_string();
                round.finalized_at = now;
                inner.validator_history.insert(
                    round_id_owned.clone(),
                    ValidatorSetRecord {
                        round_id: round_id_owned,
                        validators: validators.clone(),
                        total_bonded: total,
                        selected_at: now,
                        eligible_node: validators.len() as i64,
                    },
                );
            }
            _ => {}
        }
    }
}

fn election_meta(kvs: &[(&str, &str)]) -> HashMap<String, Value> {
    kvs.iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
        .collect()
}

// Suppress unused-import warnings on `AnyVal`/`TimeZone` for future use.
const _: fn() -> Option<AnyVal> = || None;
const _: fn() -> Option<chrono::DateTime<Utc>> = || Some(Utc.timestamp_opt(0, 0).single()?);

#[cfg(test)]
mod leak_repro {
    //! Real-code reproduction of the `message_callbacks` leak, driven through
    //! the actual `Globe::send_typed_message_on_chain`. The message-callback
    //! sink here stands in for `CoreOrchestrator::message_callbacks` (the map
    //! the real `set_message_cb_fn` closure writes into); process RSS is read
    //! the same way the node's pprof `/debug/pprof/heap` endpoint reads it.

    use super::*;

    /// VmRSS in KiB from /proc/self/status — the number pprof's heap endpoint
    /// surfaces. Returns 0 on platforms without procfs.
    fn rss_kib() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines().find_map(|l| {
                    let rest = l.strip_prefix("VmRSS:")?;
                    rest.split_whitespace().next()?.parse::<u64>().ok()
                })
            })
            .unwrap_or(0)
    }

    /// Build a `Globe` whose `set_message_cb_fn` records into `sink` — exactly
    /// what the orchestrator's real closure does into `message_callbacks`.
    fn globe_with_sink(sink: Arc<Mutex<HashMap<String, MessageCallback>>>) -> Arc<Globe> {
        let peers_fn: PeersFn = Arc::new(Vec::new);
        let sign_fn: SignPacketFn = Arc::new(|_| String::new());
        let submit_fn: SubmitChainPacketFn = Arc::new(|_, _| {});
        let set_chain_cb: SetChainCallbackFn = Arc::new(|_, _| {});
        let set_msg_cb: SetMessageCbFn = {
            let sink = sink.clone();
            Arc::new(move |id: &str, cb: MessageCallback| {
                sink.lock().unwrap().insert(id.to_string(), cb);
            })
        };
        Globe::new(
            "node1".into(),
            "node1".into(),
            peers_fn,
            sign_fn,
            submit_fn,
            set_chain_cb,
            set_msg_cb,
            50,
            120,
            120,
        )
    }

    fn fire(globe: &Globe, callback: Option<TypedMessageCallback>) {
        let mut receivers: HashMap<String, HashMap<String, bool>> = HashMap::new();
        receivers.insert("*".to_string(), HashMap::new());
        globe.send_typed_message_on_chain(
            "main",
            "chains/vm/request",
            "vm.chain",
            b"{}".to_vec(),
            "",
            "owner",
            receivers,
            "",
            "store1",
            None,
            callback,
        );
    }

    // The fix: fire-and-forget sends pass `None`, so nothing is parked in the
    // callback sink no matter how many messages are sent.
    #[test]
    fn fire_and_forget_none_does_not_grow_callback_map() {
        let sink: Arc<Mutex<HashMap<String, MessageCallback>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let globe = globe_with_sink(sink.clone());

        let n = 200_000;
        let rss_before = rss_kib();
        for _ in 0..n {
            fire(&globe, None);
        }
        let rss_after = rss_kib();

        let len = sink.lock().unwrap().len();
        eprintln!(
            "[none] sent={} parked_callbacks={} rss_kib {} -> {} (delta {})",
            n,
            len,
            rss_before,
            rss_after,
            rss_after as i64 - rss_before as i64,
        );
        assert_eq!(len, 0, "fire-and-forget must not park any callback");
    }

    // Counterpart proving the sink genuinely reflects registrations: a bounded
    // batch of `Some` sends parks exactly that many callbacks. Before the fix
    // every send (all fire-and-forget in the tree) took this branch and nothing
    // ever removed the entries — measured at ~309 B/entry, i.e. ~1.2 GB per ~4M
    // messages, the observed production growth. `Some` callbacks are now
    // one-shot (removed on reply delivery in the orchestrator).
    #[test]
    fn registered_some_parks_one_callback_each() {
        let sink: Arc<Mutex<HashMap<String, MessageCallback>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let globe = globe_with_sink(sink.clone());

        let n = 1_000;
        for _ in 0..n {
            fire(&globe, Some(Box::new(|_, _| {})));
        }
        assert_eq!(
            sink.lock().unwrap().len(),
            n,
            "each registered send parks exactly one callback (the pre-fix leak)"
        );
    }
}
