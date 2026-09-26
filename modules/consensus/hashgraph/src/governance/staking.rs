//! Validator staking ledger: bond/unbond/slash and mature-unbond settlement.
//!
//! Moved from the node's legacy globe (`core/module/globe`) into the consensus
//! engine (RL-011): the staking rules are governance, not node orchestration.

use std::collections::HashMap;

use aseman_domain::consensus::{StakeAction, StakeRequest, ValidatorStake};

pub const MIN_VALIDATOR_STAKE: i64 = 1_000;
pub const MAX_VALIDATOR_STAKE: i64 = 1_000_000_000_000;
pub const UNBONDING_SECONDS: i64 = 24 * 60 * 60;

/// Wire action names the staking ledger understands.
pub const STAKE_ACTION_BOND: &str = "bond";
pub const STAKE_ACTION_UNBOND: &str = "unbond";
pub const STAKE_ACTION_SLASH: &str = "slash";

/// The staking ledger: per-node stake state plus the derived totals.
#[derive(Default)]
pub struct StakingState {
    node_states: HashMap<String, ValidatorStake>,
    node_owners: HashMap<String, String>,
    node_stakes: HashMap<String, i64>,
    total_bonded_stake: i64,
    min_validator_stake: i64,
    max_validator_stake: i64,
    unbonding_seconds: i64,
}

impl StakingState {
    pub fn new() -> StakingState {
        StakingState {
            min_validator_stake: MIN_VALIDATOR_STAKE,
            max_validator_stake: MAX_VALIDATOR_STAKE,
            unbonding_seconds: UNBONDING_SECONDS,
            ..StakingState::default()
        }
    }

    /// Configure the minimum bonded stake for election eligibility.
    pub fn set_min_validator_stake(&mut self, value: i64) {
        self.min_validator_stake = value;
    }

    /// Configure the cap on a single node's bonded stake.
    pub fn set_max_validator_stake(&mut self, value: i64) {
        self.max_validator_stake = value;
    }

    /// Configure the unbond maturation window in seconds.
    pub fn set_unbonding_seconds(&mut self, value: i64) {
        self.unbonding_seconds = value;
    }

    /// Apply a stake request. Returns `true` if the request was accepted (a
    /// malformed request or a nonce regression is refused).
    pub fn apply(&mut self, request: &StakeRequest, now: i64) -> bool {
        if request.node_id.is_empty() || request.owner_id.is_empty() || request.amount <= 0 {
            return true;
        }
        self.settle_mature_unbonds(now);

        let state = self
            .node_states
            .entry(request.node_id.clone())
            .or_insert_with(|| ValidatorStake {
                node_id: request.node_id.clone(),
                owner_id: request.owner_id.clone(),
                ..ValidatorStake::default()
            });
        if !state.owner_id.is_empty() && state.owner_id != request.owner_id {
            return true;
        }
        if request.nonce > 0 && request.nonce <= state.nonce {
            return true;
        }
        if request.nonce > 0 {
            state.nonce = request.nonce;
        }
        state.owner_id = request.owner_id.clone();

        let owner = state.owner_id.clone();

        let max_stake = self.max_validator_stake;
        let unbonding_seconds = self.unbonding_seconds;
        match request.action {
            StakeAction::Bond => {
                Self::apply_bond(state, request.amount, request.lock_seconds, now, max_stake)
            }
            StakeAction::Unbond => {
                Self::apply_unbond(state, request.amount, now, unbonding_seconds)
            }
            StakeAction::Slash => Self::apply_slash(state, request.amount, now, unbonding_seconds),
        }
        state.last_updated_at = now;
        let bonded = state.bonded_stake;

        // Re-derive totals from the state map (rare path, cheap).
        self.total_bonded_stake = self.node_states.values().map(|s| s.bonded_stake).sum();
        self.node_owners.insert(request.node_id.clone(), owner);
        self.node_stakes.insert(request.node_id.clone(), bonded);
        true
    }

    /// The staked node ids -> bonded stake snapshot.
    pub fn snapshot(&self) -> HashMap<String, i64> {
        self.node_stakes.clone()
    }

    /// The next stake nonce for `node_id`: the current nonce plus one, or `1`
    /// when the node has never staked.
    pub fn next_nonce(&self, node_id: &str) -> u64 {
        self.node_states
            .get(node_id)
            .map(|state| state.nonce + 1)
            .unwrap_or(1)
    }

    fn apply_bond(
        state: &mut ValidatorStake,
        amount: i64,
        lock_seconds: i64,
        now: i64,
        max_stake: i64,
    ) {
        if amount <= 0 {
            return;
        }
        let mut new_bonded = state.bonded_stake + amount;
        if new_bonded > max_stake {
            new_bonded = max_stake;
        }
        state.bonded_stake = new_bonded;
        if lock_seconds > 0 {
            let lock_until = now + lock_seconds;
            if lock_until > state.unbond_unlock_at {
                state.unbond_unlock_at = lock_until;
            }
        }
    }

    fn apply_unbond(state: &mut ValidatorStake, amount: i64, now: i64, unbonding_seconds: i64) {
        if amount <= 0 || state.bonded_stake <= 0 || now < state.unbond_unlock_at {
            return;
        }
        let amount = amount.min(state.bonded_stake);
        state.bonded_stake -= amount;
        state.pending_unbond += amount;
        state.unbond_unlock_at = now + unbonding_seconds;
    }

    fn apply_slash(state: &mut ValidatorStake, amount: i64, now: i64, unbonding_seconds: i64) {
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
        state.unbond_unlock_at = now + unbonding_seconds;
    }

    fn settle_mature_unbonds(&mut self, now: i64) {
        for state in self.node_states.values_mut() {
            if state.pending_unbond > 0 && now >= state.unbond_unlock_at {
                state.pending_unbond = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(node: &str, owner: &str, action: StakeAction, amount: i64) -> StakeRequest {
        StakeRequest {
            node_id: node.to_string(),
            owner_id: owner.to_string(),
            action,
            amount,
            nonce: 1,
            lock_seconds: 0,
        }
    }

    #[test]
    fn bond_unbond_slash_round_trip() {
        let mut s = StakingState::new();
        // Start after the unbonding window so unbond/slash can take effect.
        let t0 = 1_000_000;
        let mut r = req("a", "o", StakeAction::Bond, 10_000);
        r.nonce = 1;
        s.apply(&r, t0);
        assert_eq!(s.node_stakes.get("a"), Some(&10_000));
        let mut r = req("a", "o", StakeAction::Unbond, 4_000);
        r.nonce = 2;
        s.apply(&r, t0 + UNBONDING_SECONDS + 1);
        assert_eq!(s.node_stakes.get("a"), Some(&6_000));
        let mut r = req("a", "o", StakeAction::Slash, 1_000);
        r.nonce = 3;
        s.apply(&r, t0 + UNBONDING_SECONDS + 2);
        assert_eq!(s.node_stakes.get("a"), Some(&5_000));
    }

    #[test]
    fn cap_at_max_validator_stake() {
        let mut s = StakingState::new();
        let mut r = req("a", "o", StakeAction::Bond, MAX_VALIDATOR_STAKE + 1);
        r.nonce = 1;
        s.apply(&r, 0);
        assert_eq!(s.node_stakes.get("a"), Some(&MAX_VALIDATOR_STAKE));
    }

    #[test]
    fn nonce_regression_is_refused() {
        let mut s = StakingState::new();
        let mut r = req("a", "o", StakeAction::Bond, 1_000);
        r.nonce = 5;
        s.apply(&r, 1_000_000);
        r.nonce = 3;
        // An older nonce is refused: the packet is handled but nothing changes.
        s.apply(&r, 1_000_001);
        assert_eq!(s.node_stakes.get("a"), Some(&1_000));
    }
}
