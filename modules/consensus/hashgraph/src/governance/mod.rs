//! Validator-set governance: staking and the weighted election.
//!
//! Consensus decides *who* may finalize records; staking decides *how much* each
//! candidate is weighted, and the election picks the weighted validator set for a
//! round. This subsystem owns those state machines inside the consensus engine,
//! including the commit/reveal packet flow: it is driven by incoming chain packets
//! and emits its own outbound election packets through an injected submit hook.
//!
//! The core does **not** drive governance through a fixed per-feature API. It writes
//! environment-style configuration pairs via [`ConsensusProvider::set`], and each
//! provider interprets the keys it understands. This backend understands `staking.*`
//! and `election.*` keys; a provider without staking simply refuses those keys.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aseman_contracts::wire::chain::{ChainElectionPacket, ChainStakePacket};
use aseman_domain::consensus::{ElectedValidators, StakeAction, StakeRequest};
use aseman_ports::{PortError, PortResult};
use serde_json::Value;

mod election;
mod staking;

use crate::governance::election::ElectionState;
use crate::governance::staking::StakingState;

pub use crate::governance::staking::{STAKE_ACTION_BOND, STAKE_ACTION_SLASH, STAKE_ACTION_UNBOND};

/// Submit hook for outbound election packets, injected by the node's chain edge.
pub type SubmitElectionFn = Arc<dyn Fn(ChainElectionPacket) + Send + Sync>;

/// Governance state for one node: the staking ledger plus the election round.
///
/// Both halves are guarded by the same mutex because the election reads the
/// staking totals when it finalizes a round.
pub struct Governance {
    node_id: String,
    voter_id: String,
    submit: Mutex<Option<SubmitElectionFn>>,
    inner: Mutex<Inner>,
}

struct Inner {
    staking: StakingState,
    election: ElectionState,
    election_commit_secs: i64,
    election_reveal_secs: i64,
}

impl Governance {
    /// Create empty governance state for `node_id` (the node's id) and
    /// `voter_id` (the identity that casts the node's vote in elections).
    pub fn new(node_id: &str, voter_id: &str) -> Governance {
        Governance {
            node_id: node_id.to_string(),
            voter_id: voter_id.to_string(),
            submit: Mutex::new(None),
            inner: Mutex::new(Inner {
                staking: StakingState::new(),
                election: ElectionState::new(node_id),
                election_commit_secs: 120,
                election_reveal_secs: 120,
            }),
        }
    }

    /// Inject the outbound election-packet submit hook (the node's chain edge).
    pub fn set_submit(&self, submit: SubmitElectionFn) {
        *self.submit.lock().unwrap() = Some(submit);
    }

    /// Apply a configuration pair, environment-style. Keys are provider-specific;
    /// the ones this backend understands configure staking and election knobs.
    ///
    /// Supported keys:
    /// - `staking.min_validator_stake` — minimum bonded stake to be eligible.
    /// - `staking.max_validator_stake` — cap on a single node's bonded stake.
    /// - `staking.unbonding_seconds` — unbond maturation window.
    /// - `election.max_validator_count` — cap on the elected validator set.
    /// - `election.commit_seconds` — window before the reveal phase starts.
    /// - `election.reveal_seconds` — window before the round finalizes.
    pub fn set(&self, key: &str, value: &str) -> PortResult<()> {
        let mut inner = self.inner.lock().unwrap();
        let parsed: i64 = value
            .parse()
            .map_err(|_| PortError::Failed(format!("invalid integer value for {key}: {value}")))?;
        match key {
            "staking.min_validator_stake" => inner.staking.set_min_validator_stake(parsed),
            "staking.max_validator_stake" => inner.staking.set_max_validator_stake(parsed),
            "staking.unbonding_seconds" => inner.staking.set_unbonding_seconds(parsed),
            "election.max_validator_count" => inner.election.set_max_validator_count(parsed),
            "election.commit_seconds" => inner.election_commit_secs = parsed.max(0),
            "election.reveal_seconds" => inner.election_reveal_secs = parsed.max(0),
            _ => return Err(PortError::Unsupported("consensus governance property")),
        }
        Ok(())
    }

    /// Apply a stake packet (bond/unbond/slash) that arrived on the chain.
    ///
    /// Returns `true` when the packet was handled (accepted or a no-op refusal).
    pub fn handle_stake(&self, packet: &ChainStakePacket) -> bool {
        let action = if packet.action.is_empty() {
            StakeAction::Bond
        } else {
            match packet.action.as_str() {
                "unbond" => StakeAction::Unbond,
                "slash" => StakeAction::Slash,
                _ => StakeAction::Bond,
            }
        };
        let request = StakeRequest {
            node_id: packet.node_id.clone(),
            owner_id: packet.owner_id.clone(),
            action,
            amount: packet.amount,
            nonce: packet.nonce,
            lock_seconds: packet.lock_seconds,
        };
        let mut inner = self.inner.lock().unwrap();
        let now = now_secs();
        inner.staking.apply(&request, now)
    }

    /// The next stake nonce for `node_id` (monotonic per node).
    pub fn next_stake_nonce(&self, node_id: &str) -> u64 {
        self.inner.lock().unwrap().staking.next_nonce(node_id)
    }

    /// Handle an election packet that arrived on the chain, advancing the round.
    /// Emits outbound commit/reveal packets through the injected submit hook.
    pub fn handle_election(&self, packet: &ChainElectionPacket) -> bool {
        let phase = packet
            .meta
            .get("phase")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let now = now_secs();
        let mut inner = self.inner.lock().unwrap();

        match phase {
            "start-round" => {
                let round_id = packet
                    .meta
                    .get("roundId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let commit_seed = packet
                    .meta
                    .get("commitSeed")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let my_seed = crate::governance::election::unique_seed();
                let commit = inner.election.start_round(round_id, commit_seed, &my_seed);
                if let Some(commit) = commit {
                    self.emit(
                        &mut inner,
                        &[
                            ("phase", "commit"),
                            ("roundId", round_id),
                            ("nodeId", &self.node_id),
                            ("commit", &commit),
                        ],
                    );
                }
            }
            "commit" => {
                let round_id = packet
                    .meta
                    .get("roundId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let node_id = packet
                    .meta
                    .get("nodeId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let commit = packet
                    .meta
                    .get("commit")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                inner.election.record_commit(round_id, node_id, commit);
            }
            "start-reveal" => {
                let round_id = packet
                    .meta
                    .get("roundId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if let Some(my_seed) = inner.election.start_reveal(round_id) {
                    self.emit(
                        &mut inner,
                        &[
                            ("phase", "reveal"),
                            ("roundId", round_id),
                            ("nodeId", &self.node_id),
                            ("seed", &my_seed),
                        ],
                    );
                }
            }
            "reveal" => {
                let round_id = packet
                    .meta
                    .get("roundId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let node_id = packet
                    .meta
                    .get("nodeId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let seed = packet
                    .meta
                    .get("seed")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                inner.election.record_reveal(round_id, node_id, seed);
            }
            "finalize" => {
                let round_id = packet
                    .meta
                    .get("roundId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let stakes = inner.staking.snapshot();
                let peers: Vec<String> = stakes.keys().cloned().collect();
                let _ = inner.election.finalize(round_id, &stakes, &peers, now);
            }
            _ => return false,
        }
        true
    }

    /// Start a scheduled election for the given hour if one is due: emits the
    /// start-round packet and schedules the reveal/finalize phases.
    pub fn start_scheduled_election(&self, now: SystemTime) -> Option<String> {
        let (round_id, commit_secs, reveal_secs) = {
            let mut inner = self.inner.lock().unwrap();
            let round_id = inner.election.may_start(now)?;
            let commit_seed = crate::governance::election::unique_seed();
            let my_seed = crate::governance::election::unique_seed();
            let commit = inner
                .election
                .start_round(&round_id, &commit_seed, &my_seed);
            let commit_secs = inner.election_commit_secs;
            let reveal_secs = inner.election_reveal_secs;
            let voter = self.voter_id.clone();
            self.emit(
                &mut inner,
                &[
                    ("phase", "start-round"),
                    ("roundId", &round_id),
                    ("voter", &voter),
                    ("commitSeed", &commit_seed),
                ],
            );
            if let Some(commit) = commit {
                let node = self.node_id.clone();
                self.emit(
                    &mut inner,
                    &[
                        ("phase", "commit"),
                        ("roundId", &round_id),
                        ("nodeId", &node),
                        ("commit", &commit),
                    ],
                );
            }
            (round_id, commit_secs, reveal_secs)
        };

        // Schedule the reveal and finalize phases.
        let submit = self.submit.lock().unwrap().clone();
        let node_id = self.node_id.clone();
        let round_for_thread = round_id.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(commit_secs.max(0) as u64));
            if let Some(submit) = submit.as_ref() {
                submit(election_packet(&[
                    ("phase", "start-reveal"),
                    ("roundId", &round_for_thread),
                ]));
            }
            thread::sleep(Duration::from_secs(reveal_secs.max(0) as u64));
            if let Some(submit) = submit.as_ref() {
                submit(election_packet(&[
                    ("phase", "finalize"),
                    ("roundId", &round_for_thread),
                ]));
            }
            let _ = node_id;
        });
        Some(round_id)
    }

    /// The staked, non-empty node states for a round (node id -> bonded stake).
    pub fn staking_snapshot(&self) -> HashMap<String, i64> {
        self.inner.lock().unwrap().staking.snapshot()
    }

    /// The elected validator set, if a round has finalized.
    pub fn elected_validators(&self) -> Option<ElectedValidators> {
        self.inner.lock().unwrap().election.last_elected()
    }

    /// Emit an election packet through the submit hook. The mutex must be held.
    fn emit(&self, _inner: &mut Inner, kvs: &[(&str, &str)]) {
        if let Some(submit) = self.submit.lock().unwrap().as_ref() {
            submit(election_packet(kvs));
        }
    }
}

/// Build the `meta` map for an election packet from key/value pairs.
pub(crate) fn election_packet(kvs: &[(&str, &str)]) -> ChainElectionPacket {
    ChainElectionPacket {
        typ: "election".to_string(),
        key: "choose-validator".to_string(),
        meta: kvs
            .iter()
            .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
            .collect(),
        payload: b"{}".to_vec(),
    }
}

/// Current Unix time in seconds.
pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stake(node: &str, action: &str, amount: i64) -> ChainStakePacket {
        ChainStakePacket {
            node_id: node.to_string(),
            owner_id: "owner".to_string(),
            action: action.to_string(),
            amount,
            nonce: 1,
            lock_seconds: 0,
            reason: String::new(),
            timestamp: now_secs(),
        }
    }

    fn election(phase: &str, round: &str) -> ChainElectionPacket {
        ChainElectionPacket {
            typ: "election".to_string(),
            key: "choose-validator".to_string(),
            meta: serde_json::Map::from_iter([
                ("phase".to_string(), json!(phase)),
                ("roundId".to_string(), json!(round)),
            ])
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>(),
            payload: Vec::new(),
        }
    }

    #[test]
    fn bond_increases_snapshot() {
        let g = Governance::new("node1", "voter1");
        assert!(g.handle_stake(&stake("a", "bond", 5_000)));
        assert_eq!(g.staking_snapshot().get("a"), Some(&5_000));
    }

    #[test]
    fn nonce_is_monotonic() {
        let g = Governance::new("node1", "voter1");
        g.handle_stake(&stake("a", "bond", 5_000));
        assert!(g.next_stake_nonce("a") >= 1);
    }

    #[test]
    fn election_start_round_emits_commit() {
        let g = Governance::new("node1", "voter1");
        let emitted = Arc::new(Mutex::new(Vec::new()));
        let sink = emitted.clone();
        g.set_submit(Arc::new(move |pkt| sink.lock().unwrap().push(pkt)));
        assert!(g.handle_election(&election("start-round", "r1")));
        let packets = emitted.lock().unwrap();
        assert!(!packets.is_empty(), "commit packet must be emitted");
        assert_eq!(
            packets[0].meta.get("phase").and_then(|v| v.as_str()),
            Some("commit")
        );
    }

    #[test]
    fn unknown_phase_is_refused() {
        let g = Governance::new("node1", "voter1");
        assert!(!g.handle_election(&election("bogus", "r1")));
    }

    #[test]
    fn config_keys_apply() {
        let g = Governance::new("node1", "voter1");
        g.set("staking.min_validator_stake", "2").unwrap();
        g.set("election.max_validator_count", "10").unwrap();
        g.set("election.commit_seconds", "5").unwrap();
        g.set("election.reveal_seconds", "5").unwrap();
        assert_eq!(
            g.set("unknown.key", "1"),
            Err(PortError::Unsupported("consensus governance property"))
        );
    }
}
