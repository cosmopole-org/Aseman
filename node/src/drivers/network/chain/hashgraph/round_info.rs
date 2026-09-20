//! Translation of `chain/hashgraph/roundInfo.go`.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::drivers::network::chain::common::Trilean;
use crate::drivers::network::chain::peers::PeerSet;

/// Translation of the Go `pendingRound` type (unexported and unused in the
/// original; the round-through-consensus type the hashgraph uses is
/// `caches::PendingRound`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PendingRoundMarker {
    pub index: i64,
    pub decided: bool,
}

/// Indicates the witness and fame states of an Event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct RoundEvent {
    pub witness: bool,
    pub famous: Trilean,
}

/// Encapsulates information about a round.
///
/// `created_events` is a `BTreeMap` so that marshalling is deterministic,
/// matching the Go code's canonical codec handle.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct RoundInfo {
    /// The events that were "created" in this round.
    pub created_events: BTreeMap<String, RoundEvent>,
    /// The events that were "received" in this round.
    pub received_events: Vec<String>,
    #[serde(skip)]
    pub queued: bool,
    #[serde(skip)]
    pub decided: bool,
}

impl RoundInfo {
    /// Creates a new `RoundInfo`.
    pub fn new() -> RoundInfo {
        RoundInfo {
            created_events: BTreeMap::new(),
            received_events: Vec::new(),
            queued: false,
            decided: false,
        }
    }

    /// Adds an event to the `created_events` map.
    pub fn add_created_event(&mut self, x: &str, witness: bool) {
        self.created_events
            .entry(x.to_string())
            .or_insert(RoundEvent {
                witness,
                famous: Trilean::Undefined,
            });
    }

    /// Adds an event to the `received_events` list.
    pub fn add_received_event(&mut self, x: &str) {
        self.received_events.push(x.to_string());
    }

    /// Sets the famous status of an event.
    pub fn set_fame(&mut self, x: &str, f: bool) {
        let mut e = self.created_events.get(x).copied().unwrap_or(RoundEvent {
            witness: true,
            famous: Trilean::Undefined,
        });
        e.famous = if f { Trilean::True } else { Trilean::False };
        self.created_events.insert(x.to_string(), e);
    }

    /// Returns true if a super-majority of witnesses are decided and there are
    /// no undecided witnesses. Once a Round is decided it stays decided.
    pub fn witnesses_decided(&mut self, peer_set: &PeerSet) -> bool {
        // if the round was already decided, it stays decided no matter what.
        if self.decided {
            return true;
        }

        let mut c: i64 = 0;
        for e in self.created_events.values() {
            if e.witness && e.famous != Trilean::Undefined {
                c += 1;
            } else if e.witness && e.famous == Trilean::Undefined {
                return false;
            }
        }

        self.decided = c >= peer_set.super_majority();
        self.decided
    }

    /// Returns the round's witnesses.
    pub fn witnesses(&self) -> Vec<String> {
        self.created_events
            .iter()
            .filter(|(_, e)| e.witness)
            .map(|(x, _)| x.clone())
            .collect()
    }

    /// Returns the round's famous witnesses.
    pub fn famous_witnesses(&self) -> Vec<String> {
        self.created_events
            .iter()
            .filter(|(_, e)| e.witness && e.famous == Trilean::True)
            .map(|(x, _)| x.clone())
            .collect()
    }

    /// Returns true unless the famous status of `witness` is undecided.
    pub fn is_decided(&self, witness: &str) -> bool {
        match self.created_events.get(witness) {
            Some(w) => w.witness && w.famous != Trilean::Undefined,
            None => false,
        }
    }

    /// Returns the JSON encoding of a `RoundInfo`.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    /// Parses a JSON encoded `RoundInfo`.
    pub fn unmarshal(&mut self, data: &[u8]) -> Result<()> {
        *self = serde_json::from_slice(data)?;
        Ok(())
    }

    /// Returns true if the `RoundInfo` is marked as queued.
    pub fn is_queued(&self) -> bool {
        self.queued
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::network::chain::crypto::keys::{generate_ecdsa_key, public_key_hex};
    use crate::drivers::network::chain::peers::Peer;

    fn make_peer_set(n: usize) -> PeerSet {
        let peers: Vec<Peer> = (0..n)
            .map(|i| {
                let key = generate_ecdsa_key().unwrap();
                Peer::new(
                    &public_key_hex(key.verifying_key()),
                    &format!("addr{}", i),
                    &format!("m{}", i),
                )
            })
            .collect();
        PeerSet::new(peers)
    }

    #[test]
    fn new_round_info_is_empty_and_default_undecided() {
        let r = RoundInfo::new();
        assert!(r.created_events.is_empty());
        assert!(r.received_events.is_empty());
        assert!(!r.decided);
        assert!(!r.is_queued());
    }

    #[test]
    fn add_created_event_does_not_overwrite_existing_entry() {
        let mut r = RoundInfo::new();
        r.add_created_event("e1", true);
        // Mark famous: this should be preserved by a subsequent add that
        // tries to re-insert with `witness: false`.
        r.set_fame("e1", true);
        r.add_created_event("e1", false);
        let e = r.created_events.get("e1").unwrap();
        assert!(e.witness, "witness flag should be preserved");
        assert_eq!(e.famous, Trilean::True);
    }

    #[test]
    fn add_received_event_appends() {
        let mut r = RoundInfo::new();
        r.add_received_event("a");
        r.add_received_event("b");
        assert_eq!(r.received_events, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn set_fame_records_true_and_false() {
        let mut r = RoundInfo::new();
        r.add_created_event("w", true);
        r.set_fame("w", true);
        assert_eq!(r.created_events.get("w").unwrap().famous, Trilean::True);
        r.set_fame("w", false);
        assert_eq!(r.created_events.get("w").unwrap().famous, Trilean::False);
    }

    #[test]
    fn set_fame_creates_entry_for_unknown_witness() {
        let mut r = RoundInfo::new();
        r.set_fame("w", true);
        let e = r.created_events.get("w").unwrap();
        assert!(e.witness);
        assert_eq!(e.famous, Trilean::True);
    }

    #[test]
    fn is_decided_only_true_for_decided_witness() {
        let mut r = RoundInfo::new();
        r.add_created_event("w", true);
        assert!(!r.is_decided("w"), "undefined should not be decided");
        r.set_fame("w", false);
        assert!(r.is_decided("w"));
        assert!(!r.is_decided("missing"));
    }

    #[test]
    fn witnesses_returns_only_witness_entries() {
        let mut r = RoundInfo::new();
        r.add_created_event("w1", true);
        r.add_created_event("e1", false);
        r.add_created_event("w2", true);
        let mut w = r.witnesses();
        w.sort();
        assert_eq!(w, vec!["w1".to_string(), "w2".to_string()]);
    }

    #[test]
    fn famous_witnesses_returns_only_true_fame() {
        let mut r = RoundInfo::new();
        r.add_created_event("w1", true);
        r.add_created_event("w2", true);
        r.add_created_event("w3", true);
        r.set_fame("w1", true);
        r.set_fame("w2", false);
        // w3 remains undefined
        let f = r.famous_witnesses();
        assert_eq!(f, vec!["w1".to_string()]);
    }

    #[test]
    fn witnesses_decided_requires_super_majority() {
        let peer_set = make_peer_set(4); // super_majority = 3
        let mut r = RoundInfo::new();
        r.add_created_event("w1", true);
        r.add_created_event("w2", true);
        r.add_created_event("w3", true);

        // No fame set: there are undefined witnesses -> not decided.
        assert!(!r.clone().witnesses_decided(&peer_set));

        r.set_fame("w1", true);
        r.set_fame("w2", true);
        // Still one undefined -> not decided.
        assert!(!r.clone().witnesses_decided(&peer_set));

        r.set_fame("w3", true);
        assert!(r.witnesses_decided(&peer_set));
    }

    #[test]
    fn decided_stays_decided_even_if_new_undecided_witness_added() {
        let peer_set = make_peer_set(4);
        let mut r = RoundInfo::new();
        for w in ["w1", "w2", "w3"] {
            r.add_created_event(w, true);
            r.set_fame(w, true);
        }
        assert!(r.witnesses_decided(&peer_set));

        // Add a fresh undecided witness; "decided" is sticky.
        r.add_created_event("w4", true);
        assert!(r.witnesses_decided(&peer_set));
    }

    #[test]
    fn marshal_unmarshal_round_trip_preserves_persistent_fields() {
        let mut r = RoundInfo::new();
        r.add_created_event("w1", true);
        r.set_fame("w1", true);
        r.add_received_event("r1");
        r.queued = true; // not serialized
        r.decided = true; // not serialized

        let bytes = r.marshal().unwrap();
        let mut restored = RoundInfo::new();
        restored.unmarshal(&bytes).unwrap();

        assert_eq!(restored.created_events, r.created_events);
        assert_eq!(restored.received_events, r.received_events);
        // queued and decided are skipped serialize; restored defaults to false.
        assert!(!restored.queued);
        assert!(!restored.decided);
    }
}
