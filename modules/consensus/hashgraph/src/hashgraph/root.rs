//! Translation of `chain/hashgraph/root.go`.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::event::FrameEvent;
use crate::common;
use crate::crypto;

/// Forms a base on top of which a participant's Events can be inserted. It
/// contains `FrameEvent`s sorted by Lamport timestamp.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Root {
    pub events: Vec<FrameEvent>,
}

impl Root {
    /// Instantiates a new empty root.
    pub fn new() -> Root {
        Root { events: Vec::new() }
    }

    /// Appends a `FrameEvent` to the root's event slice. Items are assumed to
    /// be inserted in topological order.
    pub fn insert(&mut self, frame_event: FrameEvent) {
        self.events.push(frame_event);
    }

    /// Returns the JSON encoding of a Root.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        // Go uses `json.Encoder`, which appends a trailing newline.
        let mut b = serde_json::to_vec(self)?;
        b.push(b'\n');
        Ok(b)
    }

    /// Parses a JSON encoded Root.
    pub fn unmarshal(&mut self, data: &[u8]) -> Result<()> {
        *self = serde_json::from_slice(data)?;
        Ok(())
    }

    /// Returns the SHA256 hash of the marshalled Root.
    pub fn hash(&self) -> Result<String> {
        let hash_bytes = self.marshal()?;
        let hash = crypto::sha256(&hash_bytes);
        Ok(common::encode_to_string(&hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashgraph::event::FrameEvent;

    #[test]
    fn new_root_is_empty() {
        let r = Root::new();
        assert!(r.events.is_empty());
    }

    #[test]
    fn insert_appends_events_in_order() {
        let mut r = Root::new();
        r.insert(FrameEvent::default());
        r.insert(FrameEvent::default());
        assert_eq!(r.events.len(), 2);
    }

    #[test]
    fn marshal_appends_trailing_newline() {
        let r = Root::new();
        let bytes = r.marshal().expect("marshal");
        assert_eq!(*bytes.last().unwrap(), b'\n');
    }

    #[test]
    fn marshal_unmarshal_round_trip() {
        let mut original = Root::new();
        original.insert(FrameEvent::default());
        let bytes = original.marshal().unwrap();

        let mut restored = Root::default();
        restored.unmarshal(&bytes).expect("unmarshal");
        assert_eq!(restored, original);
    }

    #[test]
    fn hash_is_hex_encoded_with_0x_prefix_and_deterministic() {
        let r = Root::new();
        let h = r.hash().unwrap();
        assert!(h.starts_with("0X"));
        // 32-byte SHA-256 -> "0X" + 64 hex chars.
        assert_eq!(h.len(), 2 + 64);
        assert_eq!(h, r.hash().unwrap());
    }

    #[test]
    fn hash_differs_after_insertion() {
        let empty = Root::new().hash().unwrap();
        let mut r = Root::new();
        r.insert(FrameEvent::default());
        assert_ne!(empty, r.hash().unwrap());
    }
}
