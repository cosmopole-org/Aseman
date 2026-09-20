//! Translation of `chain/hashgraph/event.go`.

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::Result;
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};

use super::block::{BlockSignature, WireBlockSignature};
use super::internal_transaction::InternalTransaction;
use crate::drivers::network::chain::common;
use crate::drivers::network::chain::crypto;
use crate::drivers::network::chain::crypto::keys;

use crate::util::clone_once_lock;

// ---------------------------------------------------------------------------
// EventBody
// ---------------------------------------------------------------------------

/// Contains the payload of an [`Event`] and the information that ties it to
/// other Events. The trailing fields are for local computations only and are
/// not serialized.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct EventBody {
    /// Application transactions.
    #[serde(with = "crate::util::bytes_base64_vec")]
    pub transactions: Vec<Vec<u8>>,
    /// Internal transactions (add / remove peers).
    pub internal_transactions: Vec<InternalTransaction>,
    /// Hashes of the event's parents, self-parent first.
    pub parents: Vec<String>,
    /// Creator's public key.
    #[serde(with = "crate::util::bytes_base64")]
    pub creator: Vec<u8>,
    /// Index in the sequence of events created by Creator.
    pub index: i64,
    /// Block signatures signed by the Event Creator ONLY.
    pub block_signatures: Vec<BlockSignature>,
    /// Unix timestamp (seconds) when the Event was created.
    pub timestamp: i64,

    // These fields are not serialized.
    #[serde(skip)]
    pub creator_id: u32,
    #[serde(skip)]
    pub other_parent_creator_id: u32,
    #[serde(skip)]
    pub self_parent_index: i64,
    #[serde(skip)]
    pub other_parent_index: i64,
}

impl EventBody {
    /// Returns the JSON encoding of an `EventBody`.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        // Go uses `json.Encoder`, which appends a trailing newline.
        let mut b = serde_json::to_vec(self)?;
        b.push(b'\n');
        Ok(b)
    }

    /// Parses a JSON encoded `EventBody`.
    pub fn unmarshal(&mut self, data: &[u8]) -> Result<()> {
        *self = serde_json::from_slice(data)?;
        Ok(())
    }

    /// Returns the SHA256 hash of the JSON encoded `EventBody`.
    pub fn hash(&self) -> Result<Vec<u8>> {
        Ok(crypto::sha256(&self.marshal()?))
    }
}

// ---------------------------------------------------------------------------
// CoordinatesMap
// ---------------------------------------------------------------------------

/// Combines the index and hash of an Event.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct EventCoordinates {
    pub hash: String,
    pub index: i64,
}

/// Used by the Hashgraph consensus methods to efficiently calculate the
/// `strongly_see` predicate.
pub type CoordinatesMap = HashMap<String, EventCoordinates>;

/// Creates an empty `CoordinatesMap`.
pub fn new_coordinates_map() -> CoordinatesMap {
    HashMap::new()
}

// ---------------------------------------------------------------------------
// Event
// ---------------------------------------------------------------------------

/// The fundamental unit of a Hashgraph: an [`EventBody`] plus the creator's
/// signature of that body, along with private fields used for local
/// computations and ordering.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Event {
    pub body: EventBody,
    /// Creator's digital signature of the body.
    pub signature: String,

    #[serde(skip)]
    pub topological_index: i64,

    // used for sorting
    #[serde(skip)]
    pub round: Option<i64>,
    #[serde(skip)]
    pub lamport_timestamp: Option<i64>,

    #[serde(skip)]
    pub round_received: Option<i64>,

    /// `[participant pubkey] => last ancestor`.
    #[serde(skip)]
    pub last_ancestors: CoordinatesMap,
    /// `[participant pubkey] => first descendant`.
    #[serde(skip)]
    pub first_descendants: CoordinatesMap,

    #[serde(skip)]
    pub(crate) creator: OnceLock<String>,
    #[serde(skip)]
    pub(crate) hash: OnceLock<Vec<u8>>,
    #[serde(skip)]
    pub(crate) hex: OnceLock<String>,
}

impl Default for Event {
    fn default() -> Self {
        Event {
            body: EventBody::default(),
            signature: String::new(),
            topological_index: 0,
            round: None,
            lamport_timestamp: None,
            round_received: None,
            last_ancestors: new_coordinates_map(),
            first_descendants: new_coordinates_map(),
            creator: OnceLock::new(),
            hash: OnceLock::new(),
            hex: OnceLock::new(),
        }
    }
}

impl Clone for Event {
    fn clone(&self) -> Self {
        Event {
            body: self.body.clone(),
            signature: self.signature.clone(),
            topological_index: self.topological_index,
            round: self.round,
            lamport_timestamp: self.lamport_timestamp,
            round_received: self.round_received,
            last_ancestors: self.last_ancestors.clone(),
            first_descendants: self.first_descendants.clone(),
            creator: clone_once_lock(&self.creator),
            hash: clone_once_lock(&self.hash),
            hex: clone_once_lock(&self.hex),
        }
    }
}

impl PartialEq for Event {
    fn eq(&self, other: &Self) -> bool {
        self.body == other.body
            && self.signature == other.signature
            && self.topological_index == other.topological_index
            && self.round == other.round
            && self.lamport_timestamp == other.lamport_timestamp
            && self.round_received == other.round_received
            && self.last_ancestors == other.last_ancestors
            && self.first_descendants == other.first_descendants
    }
}

impl Event {
    /// Instantiates a new Event.
    pub fn new(
        transactions: Vec<Vec<u8>>,
        internal_transactions: Vec<InternalTransaction>,
        block_signatures: Vec<BlockSignature>,
        parents: Vec<String>,
        creator: Vec<u8>,
        index: i64,
    ) -> Event {
        let body = EventBody {
            transactions,
            internal_transactions,
            block_signatures,
            parents,
            creator,
            index,
            timestamp: chrono::Utc::now().timestamp(),
            ..EventBody::default()
        };
        Event {
            body,
            ..Event::default()
        }
    }

    /// Returns the string representation of the creator's public key.
    pub fn creator(&self) -> String {
        self.creator
            .get_or_init(|| common::encode_to_string(&self.body.creator))
            .clone()
    }

    /// Returns the Event's self-parent.
    pub fn self_parent(&self) -> String {
        self.body.parents[0].clone()
    }

    /// Returns the Event's other-parent.
    pub fn other_parent(&self) -> String {
        self.body.parents[1].clone()
    }

    /// Returns the Event's transactions.
    pub fn transactions(&self) -> &[Vec<u8>] {
        &self.body.transactions
    }

    /// Returns the Event's internal transactions.
    pub fn internal_transactions(&self) -> &[InternalTransaction] {
        &self.body.internal_transactions
    }

    /// Returns the Event's index.
    pub fn index(&self) -> i64 {
        self.body.index
    }

    /// Returns the Event's timestamp.
    pub fn timestamp(&self) -> i64 {
        self.body.timestamp
    }

    /// Returns the Event's block signatures.
    pub fn block_signatures(&self) -> &[BlockSignature] {
        &self.body.block_signatures
    }

    /// True if the Event contains a payload or is its creator's first Event.
    pub fn is_loaded(&self) -> bool {
        if self.body.index == 0 {
            return true;
        }
        let has_transactions = !self.body.transactions.is_empty();
        let has_internal_transactions = !self.body.internal_transactions.is_empty();
        has_transactions || has_internal_transactions
    }

    /// Signs the hash of the Event's body with an ECDSA signature.
    pub fn sign(&mut self, priv_key: &SigningKey) -> Result<()> {
        let sign_bytes = self.body.hash()?;
        let (r, s) = keys::sign(priv_key, &sign_bytes)?;
        self.signature = keys::encode_signature(&r, &s);
        Ok(())
    }

    /// Verifies the Event's signature and the signatures of the internal
    /// transactions in its payload.
    pub fn verify(&self) -> Result<bool> {
        // first check signatures on internal transactions
        for itx in &self.body.internal_transactions {
            let ok = itx.verify()?;
            if !ok {
                return Err(anyhow::anyhow!("invalid signature on internal transaction"));
            }
        }

        // then check event signature
        let pub_key = match keys::to_public_key(&self.body.creator) {
            Some(pk) => pk,
            None => return Ok(false),
        };

        let sign_bytes = self.body.hash()?;
        let (r, s) = keys::decode_signature(&self.signature)?;

        Ok(keys::verify(&pub_key, &sign_bytes, &r, &s))
    }

    /// Returns the JSON encoding of the Event along with the private fields
    /// that the default marshalling would drop. Used to persist events.
    pub fn marshal_db(&self) -> Result<Vec<u8>> {
        let wrapper = EventWrapper {
            body: self.body.clone(),
            signature: self.signature.clone(),
            creator_id: self.body.creator_id,
            other_parent_creator_id: self.body.other_parent_creator_id,
            self_parent_index: self.body.self_parent_index,
            other_parent_index: self.body.other_parent_index,
            topological_index: self.topological_index,
            last_ancestors: self.last_ancestors.clone(),
            first_descendants: self.first_descendants.clone(),
        };
        Ok(serde_json::to_vec(&wrapper)?)
    }

    /// Unmarshals a JSON encoded `EventWrapper` into an Event with its private
    /// fields set.
    pub fn unmarshal_db(&mut self, data: &[u8]) -> Result<()> {
        let wrapper: EventWrapper = serde_json::from_slice(data)?;
        self.body = wrapper.body;
        self.body.creator_id = wrapper.creator_id;
        self.body.other_parent_creator_id = wrapper.other_parent_creator_id;
        self.body.self_parent_index = wrapper.self_parent_index;
        self.body.other_parent_index = wrapper.other_parent_index;
        self.signature = wrapper.signature;
        self.topological_index = wrapper.topological_index;
        self.last_ancestors = wrapper.last_ancestors;
        self.first_descendants = wrapper.first_descendants;
        self.creator = OnceLock::new();
        self.hash = OnceLock::new();
        self.hex = OnceLock::new();
        Ok(())
    }

    /// Returns the SHA256 hash of the JSON-encoded body.
    pub fn hash(&self) -> Result<Vec<u8>> {
        if let Some(h) = self.hash.get() {
            return Ok(h.clone());
        }
        let h = self.body.hash()?;
        let _ = self.hash.set(h.clone());
        Ok(h)
    }

    /// Returns a hex string representation of the Event's hash.
    pub fn hex(&self) -> String {
        if let Some(h) = self.hex.get() {
            return h.clone();
        }
        let hash = self.hash().unwrap_or_default();
        let hex = common::encode_to_string(&hash);
        let _ = self.hex.set(hex.clone());
        hex
    }

    /// Sets the Event's round number.
    pub fn set_round(&mut self, r: i64) {
        self.round = Some(r);
    }

    /// Gets the Event's round number.
    pub fn get_round(&self) -> Option<i64> {
        self.round
    }

    /// Sets the Event's Lamport timestamp.
    pub fn set_lamport_timestamp(&mut self, t: i64) {
        self.lamport_timestamp = Some(t);
    }

    /// Gets the Event's Lamport timestamp.
    pub fn get_lamport_timestamp(&self) -> Option<i64> {
        self.lamport_timestamp
    }

    /// Sets the Event's round-received (different from round).
    pub fn set_round_received(&mut self, rr: i64) {
        self.round_received = Some(rr);
    }

    /// Gets the Event's round-received.
    pub fn get_round_received(&self) -> Option<i64> {
        self.round_received
    }

    /// Sets the private fields in the Event's body used by the wire form.
    pub fn set_wire_info(
        &mut self,
        self_parent_index: i64,
        other_parent_creator_id: u32,
        other_parent_index: i64,
        creator_id: u32,
    ) {
        self.body.self_parent_index = self_parent_index;
        self.body.other_parent_creator_id = other_parent_creator_id;
        self.body.other_parent_index = other_parent_index;
        self.body.creator_id = creator_id;
    }

    /// Returns the Event's block signatures in wire format.
    pub fn wire_block_signatures(&self) -> Vec<WireBlockSignature> {
        self.body
            .block_signatures
            .iter()
            .map(|bs| bs.to_wire())
            .collect()
    }

    /// Converts an Event to its `WireEvent` representation.
    pub fn to_wire(&self) -> WireEvent {
        WireEvent {
            body: WireBody {
                transactions: self.body.transactions.clone(),
                internal_transactions: self.body.internal_transactions.clone(),
                self_parent_index: self.body.self_parent_index,
                other_parent_creator_id: self.body.other_parent_creator_id,
                other_parent_index: self.body.other_parent_index,
                creator_id: self.body.creator_id,
                index: self.body.index,
                timestamp: self.body.timestamp,
                block_signatures: self.wire_block_signatures(),
            },
            signature: self.signature.clone(),
        }
    }
}

/// Wire-form helper used by [`Event::marshal_db`] / [`Event::unmarshal_db`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct EventWrapper {
    body: EventBody,
    signature: String,
    creator_id: u32,
    other_parent_creator_id: u32,
    self_parent_index: i64,
    other_parent_index: i64,
    topological_index: i64,
    last_ancestors: CoordinatesMap,
    first_descendants: CoordinatesMap,
}

// ---------------------------------------------------------------------------
// WireEvent
// ---------------------------------------------------------------------------

/// A light-weight representation of an [`EventBody`] where hashes are replaced
/// by integers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct WireBody {
    #[serde(with = "crate::util::bytes_base64_vec")]
    pub transactions: Vec<Vec<u8>>,
    pub internal_transactions: Vec<InternalTransaction>,
    pub block_signatures: Vec<WireBlockSignature>,
    pub creator_id: u32,
    pub other_parent_creator_id: u32,
    pub index: i64,
    pub self_parent_index: i64,
    pub other_parent_index: i64,
    pub timestamp: i64,
}

/// A light-weight representation of an [`Event`] sent over the wire.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct WireEvent {
    pub body: WireBody,
    pub signature: String,
}

impl WireEvent {
    /// Unpacks `BlockSignature`s from a `WireEvent`.
    pub fn block_signatures(&self, validator: &[u8]) -> Vec<BlockSignature> {
        self.body
            .block_signatures
            .iter()
            .map(|bs| BlockSignature {
                validator: validator.to_vec(),
                index: bs.index,
                signature: bs.signature.clone(),
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// FrameEvent
// ---------------------------------------------------------------------------

/// A wrapper around a regular [`Event`] carrying its Round, Witness status and
/// Lamport timestamp.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct FrameEvent {
    /// `EventBody` + `Signature`.
    pub core: Box<Event>,
    pub round: i64,
    pub lamport_timestamp: i64,
    pub witness: bool,
}

// ---------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------
//
// Events are sorted by topological order (insertion order — node-local, not
// suitable for consensus). FrameEvents are sorted by Lamport timestamp with
// ties broken by signature comparison — this is consensus total ordering.

/// Sorts events by their private `topological_index` field.
pub fn sort_by_topological_order(events: &mut [Event]) {
    events.sort_by(|a, b| a.topological_index.cmp(&b.topological_index));
}

/// Compares two `FrameEvent`s for consensus total ordering.
pub fn compare_frame_events(a: &FrameEvent, b: &FrameEvent) -> std::cmp::Ordering {
    if a.lamport_timestamp != b.lamport_timestamp {
        return a.lamport_timestamp.cmp(&b.lamport_timestamp);
    }
    let wsi = keys::decode_signature(&a.core.signature)
        .map(|(r, _)| r)
        .unwrap_or_default();
    let wsj = keys::decode_signature(&b.core.signature)
        .map(|(r, _)| r)
        .unwrap_or_default();
    wsi.cmp(&wsj)
}

/// Sorts `FrameEvent`s by Lamport timestamp (consensus total ordering).
pub fn sort_frame_events(events: &mut [FrameEvent]) {
    events.sort_by(compare_frame_events);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::network::chain::crypto::keys::{from_public_key, generate_ecdsa_key};

    fn create_dummy_event_body() -> EventBody {
        EventBody {
            transactions: vec![b"abc".to_vec(), b"def".to_vec()],
            internal_transactions: vec![],
            parents: vec!["self".to_string(), "other".to_string()],
            creator: b"public key".to_vec(),
            block_signatures: vec![BlockSignature {
                validator: b"public key".to_vec(),
                index: 0,
                signature: "r|s".to_string(),
            }],
            ..EventBody::default()
        }
    }

    // Translation of event_test.go::TestMarshallBody.
    #[test]
    fn test_marshall_body() {
        let body = create_dummy_event_body();
        let raw = body.marshal().expect("marshal EventBody");

        let mut new_body = EventBody::default();
        new_body.unmarshal(&raw).expect("unmarshal EventBody");

        assert_eq!(body.transactions, new_body.transactions);
        assert_eq!(body.internal_transactions, new_body.internal_transactions);
        assert_eq!(body.block_signatures, new_body.block_signatures);
        assert_eq!(body.parents, new_body.parents);
        assert_eq!(body.creator, new_body.creator);
    }

    // Translation of event_test.go::TestSignEvent.
    #[test]
    fn test_sign_event() {
        let private_key = generate_ecdsa_key().unwrap();
        let public_key_bytes = from_public_key(private_key.verifying_key());

        let mut body = create_dummy_event_body();
        body.creator = public_key_bytes;

        let mut event = Event {
            body,
            ..Event::default()
        };
        event.sign(&private_key).expect("sign Event");

        let res = event.verify().expect("verify signature");
        assert!(res, "Verify returned false");
    }

    // Translation of event_test.go::TestMarshallEvent.
    #[test]
    fn test_marshall_event() {
        let private_key = generate_ecdsa_key().unwrap();
        let public_key_bytes = from_public_key(private_key.verifying_key());

        let mut body = create_dummy_event_body();
        body.creator = public_key_bytes;

        let mut event = Event {
            body,
            ..Event::default()
        };
        event.sign(&private_key).expect("sign Event");

        let raw = event.marshal_db().expect("marshal Event");

        let mut new_event = Event::default();
        new_event.unmarshal_db(&raw).expect("unmarshal Event");

        assert_eq!(new_event, event, "Events are not deeply equal");
    }

    // Translation of event_test.go::TestWireEvent.
    #[test]
    fn test_wire_event() {
        let private_key = generate_ecdsa_key().unwrap();
        let public_key_bytes = from_public_key(private_key.verifying_key());

        let mut body = create_dummy_event_body();
        body.creator = public_key_bytes;

        let mut event = Event {
            body,
            ..Event::default()
        };
        event.sign(&private_key).expect("sign Event");

        event.set_wire_info(1, 66, 2, 67);

        let expected = WireEvent {
            body: WireBody {
                transactions: event.body.transactions.clone(),
                internal_transactions: event.body.internal_transactions.clone(),
                self_parent_index: 1,
                other_parent_creator_id: 66,
                other_parent_index: 2,
                creator_id: 67,
                index: event.body.index,
                block_signatures: event.wire_block_signatures(),
                timestamp: event.body.timestamp,
            },
            signature: event.signature.clone(),
        };

        let wire_event = event.to_wire();
        assert_eq!(expected, wire_event, "WireEvent mismatch");
    }

    // Translation of event_test.go::TestIsLoaded.
    #[test]
    fn test_is_loaded() {
        // nil payload
        let mut event = Event::new(
            vec![],
            vec![],
            vec![],
            vec!["p1".to_string(), "p2".to_string()],
            b"creator".to_vec(),
            1,
        );
        assert!(!event.is_loaded(), "should be false for empty payload");

        // empty payload
        event.body.transactions = vec![];
        assert!(!event.is_loaded(), "should be false for empty transactions");

        event.body.block_signatures = vec![];
        assert!(
            !event.is_loaded(),
            "should be false for empty block signatures"
        );

        // initial event
        event.body.index = 0;
        assert!(event.is_loaded(), "should be true for initial event");

        // non-empty tx payload (Index stays 0 here, mirroring the Go test)
        event.body.transactions = vec![b"abc".to_vec()];
        assert!(event.is_loaded(), "should be true for non-empty tx payload");

        // non-empty internal-transaction payload
        event.body.transactions = vec![];
        event.body.internal_transactions = vec![InternalTransaction::default()];
        assert!(
            event.is_loaded(),
            "should be true for non-empty itx payload"
        );
    }
}
