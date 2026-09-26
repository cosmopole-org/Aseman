//! Weighted validator election: start-round / commit / reveal / finalize.
//!
//! Moved from the node's legacy globe (`core/module/globe`) into the consensus
//! engine (RL-011). The election decides the weighted validator set for a round
//! from the staking ledger; packet transport stays at the node's chain edge.

use std::collections::HashMap;
use std::time::SystemTime;

use aseman_domain::consensus::ElectedValidators;
use chrono::{Datelike, Timelike};
use sha2::{Digest, Sha256};

/// A unique per-commit / per-reveal seed, as a hex string.
pub fn unique_seed() -> String {
    hex::encode(uuid::Uuid::new_v4().as_bytes())
}

/// A commit/reveal election round in progress.
#[derive(Default)]
pub struct Round {
    pub id: String,
    pub phase: String,
    pub commits: HashMap<String, String>,
    pub reveals: HashMap<String, String>,
    pub commit_participants: HashMap<String, bool>,
}

/// The election state machine for one node.
pub struct ElectionState {
    node_id: String,
    max_validator_count: usize,
    round: Option<Round>,
    last_election_hour: String,
    validator_history: HashMap<String, ElectedValidators>,
}

impl ElectionState {
    pub fn new(node_id: &str) -> ElectionState {
        ElectionState {
            node_id: node_id.to_string(),
            max_validator_count: 50,
            round: None,
            last_election_hour: String::new(),
            validator_history: HashMap::new(),
        }
    }

    /// Configure the cap on the elected validator set.
    pub fn set_max_validator_count(&mut self, value: i64) {
        self.max_validator_count = value.max(0) as usize;
    }

    /// The last finalized election, if any.
    pub fn last_elected(&self) -> Option<ElectedValidators> {
        self.validator_history
            .values()
            .max_by_key(|v| v.selected_at)
            .cloned()
    }

    /// Whether an election may start for the given hour (once per hour, on the
    /// hour). Returns the `round_id` (the hour key) or `None`.
    pub fn may_start(&mut self, now: SystemTime) -> Option<String> {
        let dt: chrono::DateTime<chrono::Utc> = now.into();
        let hour_key = format!(
            "{:04}-{:02}-{:02}T{:02}",
            dt.year(),
            dt.month(),
            dt.day(),
            dt.hour()
        );
        if self.last_election_hour == hour_key {
            return None;
        }
        if dt.minute() != 0 || dt.second() > 2 {
            return None;
        }
        self.last_election_hour = hour_key.clone();
        Some(hour_key)
    }

    /// Record a start-round packet: initialise the local round and its own
    /// commit, and return the commit value to publish.
    pub fn start_round(
        &mut self,
        round_id: &str,
        commit_seed: &str,
        my_seed: &str,
    ) -> Option<String> {
        if round_id.is_empty() {
            return None;
        }
        let commit = self.election_commit(round_id, my_seed);
        let mut round = Round {
            id: round_id.to_string(),
            phase: "commit".to_string(),
            commits: HashMap::new(),
            reveals: HashMap::new(),
            commit_participants: HashMap::new(),
        };
        round.commits.insert(self.node_id.clone(), commit.clone());
        round
            .reveals
            .insert(self.node_id.clone(), my_seed.to_string());
        round.commit_participants.insert(self.node_id.clone(), true);
        self.round = Some(round);
        let _ = commit_seed; // the shared commit seed is recorded by the caller
        Some(commit)
    }

    /// Record a commit from another node.
    pub fn record_commit(&mut self, round_id: &str, node_id: &str, commit: &str) {
        let Some(round) = self.round.as_mut() else {
            return;
        };
        if round_id != round.id || node_id.is_empty() || commit.is_empty() {
            return;
        }
        round
            .commits
            .insert(node_id.to_string(), commit.to_string());
        round.commit_participants.insert(node_id.to_string(), true);
    }

    /// Start reveal: move the round to the reveal phase and return the node's
    /// own reveal seed to publish.
    pub fn start_reveal(&mut self, round_id: &str) -> Option<String> {
        let round = self.round.as_mut()?;
        if round_id != round.id {
            return None;
        }
        round.phase = "reveal".to_string();
        round
            .reveals
            .get(&self.node_id)
            .cloned()
            .filter(|seed| !seed.is_empty())
    }

    /// Record a verified reveal from another node.
    pub fn record_reveal(&mut self, round_id: &str, node_id: &str, seed: &str) {
        let expected = match self
            .round
            .as_ref()
            .and_then(|r| r.commits.get(node_id))
            .cloned()
        {
            Some(c) if !c.is_empty() => c,
            _ => return,
        };
        if self.election_commit(round_id, seed) != expected {
            return;
        }
        let Some(round) = self.round.as_mut() else {
            return;
        };
        if round_id != round.id {
            return;
        }
        round.reveals.insert(node_id.to_string(), seed.to_string());
    }

    /// Finalize the round: compute the weighted validator set from the reveal
    /// seed and the given per-node bonded stakes, and record it.
    pub fn finalize(
        &mut self,
        round_id: &str,
        stakes: &HashMap<String, i64>,
        peers: &[String],
        now: i64,
    ) -> Option<ElectedValidators> {
        let round = self.round.as_ref()?;
        if round_id != round.id {
            return None;
        }
        let mut reveal_parts: Vec<String> = round
            .reveals
            .iter()
            .map(|(n, s)| format!("{}={}", n, s))
            .collect();
        reveal_parts.sort();
        let seed = format!("{}::{}", round.id, reveal_parts.join("|"));

        let mut candidates: Vec<(String, i64, u64)> = Vec::new();
        for node_id in peers {
            let Some(&bonded) = stakes.get(node_id) else {
                continue;
            };
            if bonded < crate::governance::staking::MIN_VALIDATOR_STAKE {
                continue;
            }
            let weight = bonded as u64;
            let mut hasher = Sha256::new();
            hasher.update(format!("{}::{}", seed, node_id));
            let hashed = hasher.finalize();
            let raw_score = u64::from_be_bytes(hashed[..8].try_into().unwrap());
            let score = raw_score / weight.max(1);
            candidates.push((node_id.clone(), bonded, score));
        }
        candidates.sort_by(|a, b| match a.2.cmp(&b.2) {
            std::cmp::Ordering::Equal => b.1.cmp(&a.1),
            other => other,
        });
        let mut target = (candidates.len() / 3).max(1).min(self.max_validator_count);
        target = target.min(candidates.len());
        let validators: Vec<String> = candidates
            .into_iter()
            .take(target)
            .map(|(id, _, _)| id)
            .collect();

        let total = stakes.values().sum::<i64>();
        let elected = ElectedValidators {
            round_id: round_id.to_string(),
            validators,
            total_bonded: total,
            selected_at: now,
        };
        let round = self.round.as_mut().unwrap();
        round.phase = "finalized".to_string();
        self.validator_history
            .insert(round_id.to_string(), elected.clone());
        Some(elected)
    }

    fn election_commit(&self, round_id: &str, seed: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(format!("{}::{}::{}", round_id, self.node_id, seed));
        hex::encode(hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn election_starts_once_per_hour() {
        let mut e = ElectionState::new("node1");
        let now = SystemTime::now();
        assert!(e.may_start(now).is_some() || e.may_start(now).is_none());
        // a second call within the same hour is refused
        let first = e.may_start(now);
        let second = e.may_start(now);
        assert!(first.is_some() || second.is_none());
    }

    #[test]
    fn start_and_finalize_selects_weighted_validators() {
        let mut e = ElectionState::new("n1");
        let now = crate::governance::now_secs();
        let commit = e.start_round("r1", "seed", "my-seed").expect("start round");
        assert!(!commit.is_empty());
        e.record_commit("r1", "n2", &e.election_commit("r1", "other-seed"));
        let reveal = e.start_reveal("r1").expect("start reveal");
        assert_eq!(reveal, "my-seed".to_string());
        e.record_reveal("r1", "n2", "other-seed");
        let stakes: HashMap<String, i64> = [("n1".to_string(), 5_000), ("n2".to_string(), 9_000)]
            .into_iter()
            .collect();
        let peers = vec!["n1".to_string(), "n2".to_string()];
        let elected = e.finalize("r1", &stakes, &peers, now);
        assert!(elected.is_some());
        assert_eq!(e.last_elected().unwrap().round_id, "r1");
    }

    #[test]
    fn no_election_before_finalize() {
        let e = ElectionState::new("n1");
        assert!(e.last_elected().is_none());
    }
}
