//! Translation of `chain/hashgraph/frame.go`.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::event::{FrameEvent, sort_frame_events};
use super::root::Root;
use crate::crypto;
use crate::peers::Peer;

/// Represents a section of the hashgraph.
///
/// The maps use `BTreeMap` so that marshalling is deterministic, matching the
/// Go code's use of `codec.JsonHandle{Canonical: true}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Frame {
    /// RoundReceived.
    pub round: i64,
    /// The authoritative peer-set at Round.
    pub peers: Vec<Peer>,
    /// Roots on top of which Frame Events can be inserted.
    pub roots: BTreeMap<String, Root>,
    /// Events with RoundReceived = Round.
    pub events: Vec<FrameEvent>,
    /// Full peer-set history (`[round] => Peers`).
    pub peer_sets: BTreeMap<i64, Vec<Peer>>,
    /// Unix timestamp (median of round-received famous witnesses).
    pub timestamp: i64,
}

impl Frame {
    /// Returns all the events in the Frame, including events in roots, sorted
    /// by Lamport timestamp.
    pub fn sorted_frame_events(&self) -> Vec<FrameEvent> {
        let mut sorted: Vec<FrameEvent> = Vec::new();
        for r in self.roots.values() {
            sorted.extend(r.events.iter().cloned());
        }
        sorted.extend(self.events.iter().cloned());
        sort_frame_events(&mut sorted);
        sorted
    }

    /// Returns the JSON encoding of the Frame.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    /// Parses a JSON encoded Frame.
    pub fn unmarshal(&mut self, data: &[u8]) -> Result<()> {
        *self = serde_json::from_slice(data)?;
        Ok(())
    }

    /// Returns the SHA256 hash of the marshalled Frame.
    pub fn hash(&self) -> Result<Vec<u8>> {
        Ok(crypto::sha256(&self.marshal()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashgraph::event::{Event, EventBody, FrameEvent};

    fn mk_frame_event(lamport: i64) -> FrameEvent {
        FrameEvent {
            core: Box::new(Event {
                body: EventBody {
                    index: lamport,
                    ..EventBody::default()
                },
                ..Event::default()
            }),
            lamport_timestamp: lamport,
            ..FrameEvent::default()
        }
    }

    #[test]
    fn default_frame_marshals_and_hashes() {
        let f = Frame::default();
        let bytes = f.marshal().expect("marshal");
        assert!(!bytes.is_empty());
        let h = f.hash().expect("hash");
        assert_eq!(h.len(), 32);
    }

    #[test]
    fn marshal_unmarshal_round_trip() {
        let mut f = Frame {
            round: 5,
            timestamp: 100,
            events: vec![mk_frame_event(1), mk_frame_event(2)],
            ..Frame::default()
        };
        f.peer_sets.insert(0, Vec::new());

        let bytes = f.marshal().unwrap();
        let mut out = Frame::default();
        out.unmarshal(&bytes).expect("unmarshal");
        assert_eq!(out.round, f.round);
        assert_eq!(out.timestamp, f.timestamp);
        assert_eq!(out.events.len(), f.events.len());
        assert_eq!(out.peer_sets, f.peer_sets);
    }

    #[test]
    fn hash_is_deterministic_for_same_frame() {
        let f = Frame {
            round: 7,
            timestamp: 999,
            events: vec![mk_frame_event(2), mk_frame_event(1)],
            ..Frame::default()
        };
        assert_eq!(f.hash().unwrap(), f.hash().unwrap());
    }

    #[test]
    fn hash_changes_with_round() {
        let a = Frame {
            round: 1,
            ..Frame::default()
        };
        let b = Frame {
            round: 2,
            ..Frame::default()
        };
        assert_ne!(a.hash().unwrap(), b.hash().unwrap());
    }

    #[test]
    fn sorted_frame_events_orders_by_lamport_timestamp() {
        let mut f = Frame::default();
        f.events.push(mk_frame_event(5));
        f.events.push(mk_frame_event(1));
        f.events.push(mk_frame_event(3));
        let sorted = f.sorted_frame_events();
        let timestamps: Vec<i64> = sorted.iter().map(|e| e.lamport_timestamp).collect();
        for window in timestamps.windows(2) {
            assert!(window[0] <= window[1], "out of order: {:?}", timestamps);
        }
        assert_eq!(sorted.len(), 3);
    }

    #[test]
    fn sorted_frame_events_includes_root_events() {
        let mut f = Frame::default();
        let mut r = Root::default();
        r.events.push(mk_frame_event(0));
        f.roots.insert("peer1".to_string(), r);
        f.events.push(mk_frame_event(10));
        let sorted = f.sorted_frame_events();
        assert_eq!(sorted.len(), 2);
    }
}
