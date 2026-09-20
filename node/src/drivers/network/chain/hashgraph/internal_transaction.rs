//! Translation of `chain/hashgraph/internal_transaction.go`.

use anyhow::Result;
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use crate::drivers::network::chain::crypto;
use crate::drivers::network::chain::crypto::keys;
use crate::drivers::network::chain::peers::Peer;

/// Denotes the nature of an [`InternalTransaction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum TransactionType {
    /// Add a peer.
    PeerAdd = 0,
    /// Remove a peer.
    PeerRemove = 1,
}

impl Default for TransactionType {
    fn default() -> Self {
        TransactionType::PeerAdd
    }
}

impl std::fmt::Display for TransactionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TransactionType::PeerAdd => "PEER_ADD",
            TransactionType::PeerRemove => "PEER_REMOVE",
        };
        write!(f, "{}", s)
    }
}

/// Contains the payload of an [`InternalTransaction`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct InternalTransactionBody {
    /// Add or Remove.
    #[serde(rename = "Type")]
    pub typ: TransactionType,
    /// Targeted Peer.
    pub peer: Peer,
}

impl InternalTransactionBody {
    /// Returns the JSON encoding of an `InternalTransactionBody`.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        let mut b = serde_json::to_vec(self)?;
        b.push(b'\n');
        Ok(b)
    }

    /// Returns the SHA256 hash of the `InternalTransactionBody`.
    pub fn hash(&self) -> Result<Vec<u8>> {
        Ok(crypto::sha256(&self.marshal()?))
    }
}

/// A special transaction interpreted by Babble to act on its own internal
/// state (adding or removing validators). InternalTransactions also go through
/// consensus.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct InternalTransaction {
    pub body: InternalTransactionBody,
    pub signature: String,
}

impl InternalTransaction {
    /// Creates a new `InternalTransaction`.
    pub fn new(t_type: TransactionType, peer: Peer) -> InternalTransaction {
        InternalTransaction {
            body: InternalTransactionBody { typ: t_type, peer },
            signature: String::new(),
        }
    }

    /// Creates a new `InternalTransaction` to add a peer.
    pub fn new_join(peer: Peer) -> InternalTransaction {
        InternalTransaction::new(TransactionType::PeerAdd, peer)
    }

    /// Creates a new `InternalTransaction` to remove a peer.
    pub fn new_leave(peer: Peer) -> InternalTransaction {
        InternalTransaction::new(TransactionType::PeerRemove, peer)
    }

    /// Returns the JSON encoding of an `InternalTransaction`.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        let mut b = serde_json::to_vec(self)?;
        b.push(b'\n');
        Ok(b)
    }

    /// Parses an `InternalTransaction` from JSON.
    pub fn unmarshal(&mut self, data: &[u8]) -> Result<()> {
        *self = serde_json::from_slice(data)?;
        Ok(())
    }

    /// Signs the SHA256 hash of the transaction's body.
    pub fn sign(&mut self, priv_key: &SigningKey) -> Result<()> {
        let sign_bytes = self.body.hash()?;
        let (r, s) = keys::sign(priv_key, &sign_bytes)?;
        self.signature = keys::encode_signature(&r, &s);
        Ok(())
    }

    /// Verifies the transaction's signature.
    pub fn verify(&self) -> Result<bool> {
        let pub_bytes = self.body.peer.pub_key_bytes();
        let pub_key = match keys::to_public_key(&pub_bytes) {
            Some(pk) => pk,
            None => return Ok(false),
        };

        let sign_bytes = self.body.hash()?;
        let (r, s) = keys::decode_signature(&self.signature)?;

        Ok(keys::verify(&pub_key, &sign_bytes, &r, &s))
    }

    /// A string representation of the body's hash, used as a map key while an
    /// `InternalTransaction` goes through consensus.
    ///
    /// Go returned the raw hash bytes as a Go string; Rust strings must be
    /// valid UTF-8, so the translation returns the hex-encoded hash instead —
    /// an equally injective key.
    pub fn hash_string(&self) -> String {
        match self.body.hash() {
            Ok(h) => hex::encode(h),
            Err(_) => String::new(),
        }
    }

    /// Returns a receipt accepting this `InternalTransaction`.
    pub fn as_accepted(&self) -> InternalTransactionReceipt {
        InternalTransactionReceipt {
            internal_transaction: self.clone(),
            accepted: true,
        }
    }

    /// Returns a receipt refusing this `InternalTransaction`.
    pub fn as_refused(&self) -> InternalTransactionReceipt {
        InternalTransactionReceipt {
            internal_transaction: self.clone(),
            accepted: false,
        }
    }
}

/// Records the decision by the application to accept or refuse an
/// [`InternalTransaction`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct InternalTransactionReceipt {
    pub internal_transaction: InternalTransaction,
    pub accepted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::network::chain::crypto::keys::{generate_ecdsa_key, public_key_hex};

    fn fresh_peer() -> Peer {
        let key = generate_ecdsa_key().unwrap();
        Peer::new(&public_key_hex(key.verifying_key()), "addr", "moniker")
    }

    #[test]
    fn transaction_type_display_matches_go_strings() {
        assert_eq!(format!("{}", TransactionType::PeerAdd), "PEER_ADD");
        assert_eq!(format!("{}", TransactionType::PeerRemove), "PEER_REMOVE");
    }

    #[test]
    fn transaction_type_default_is_peer_add() {
        assert_eq!(TransactionType::default(), TransactionType::PeerAdd);
    }

    #[test]
    fn transaction_type_serializes_as_repr() {
        assert_eq!(
            serde_json::to_string(&TransactionType::PeerAdd).unwrap(),
            "0"
        );
        assert_eq!(
            serde_json::to_string(&TransactionType::PeerRemove).unwrap(),
            "1"
        );
    }

    #[test]
    fn body_marshal_unmarshal_round_trips_and_hashes_match() {
        let body = InternalTransactionBody {
            typ: TransactionType::PeerRemove,
            peer: fresh_peer(),
        };
        let m = body.marshal().expect("marshal");
        assert_eq!(*m.last().unwrap(), b'\n');
        let parsed: InternalTransactionBody = serde_json::from_slice(&m).expect("parse");
        assert_eq!(parsed, body);

        let h1 = body.hash().unwrap();
        let h2 = parsed.hash().unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 32);
    }

    #[test]
    fn new_join_and_new_leave_set_type_correctly() {
        let p = fresh_peer();
        let join = InternalTransaction::new_join(p.clone());
        let leave = InternalTransaction::new_leave(p);
        assert_eq!(join.body.typ, TransactionType::PeerAdd);
        assert_eq!(leave.body.typ, TransactionType::PeerRemove);
        assert!(join.signature.is_empty());
        assert!(leave.signature.is_empty());
    }

    #[test]
    fn sign_and_verify_round_trip() {
        // Use a peer whose public key is the signing key's public key, so
        // verify can recover it from the body.
        let key = generate_ecdsa_key().unwrap();
        let peer = Peer::new(&public_key_hex(key.verifying_key()), "addr", "m");
        let mut itx = InternalTransaction::new(TransactionType::PeerAdd, peer);
        itx.sign(&key).expect("sign");
        assert!(!itx.signature.is_empty(), "signature should be populated");
        assert!(itx.verify().expect("verify"));
    }

    #[test]
    fn verify_fails_with_mismatched_peer_pub_key() {
        // Sign with key A, but record peer pub key B.
        let key_a = generate_ecdsa_key().unwrap();
        let key_b = generate_ecdsa_key().unwrap();
        let peer = Peer::new(&public_key_hex(key_b.verifying_key()), "addr", "m");
        let mut itx = InternalTransaction::new(TransactionType::PeerAdd, peer);
        itx.sign(&key_a).expect("sign");
        assert!(
            !itx.verify().unwrap(),
            "verify should fail with wrong pub key"
        );
    }

    #[test]
    fn verify_returns_false_on_unparseable_pub_key() {
        let mut itx =
            InternalTransaction::new(TransactionType::PeerAdd, Peer::new("0XZZ", "addr", "m"));
        // Forge a syntactically valid signature so we exercise the pub-key
        // parse path.
        itx.signature = "1|1".to_string();
        assert!(!itx.verify().unwrap());
    }

    #[test]
    fn hash_string_is_deterministic_hex() {
        let p = fresh_peer();
        let itx = InternalTransaction::new(TransactionType::PeerAdd, p);
        let h1 = itx.hash_string();
        let h2 = itx.hash_string();
        assert_eq!(h1, h2);
        // 32-byte hash -> 64 hex characters.
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn marshal_unmarshal_round_trip() {
        let p = fresh_peer();
        let mut itx = InternalTransaction::new(TransactionType::PeerRemove, p);
        itx.signature = "deadbeef|cafebabe".to_string();

        let m = itx.marshal().expect("marshal");
        assert_eq!(*m.last().unwrap(), b'\n');
        let mut parsed = InternalTransaction::default();
        parsed.unmarshal(&m).expect("unmarshal");
        assert_eq!(parsed, itx);
    }

    #[test]
    fn as_accepted_and_as_refused_produce_correct_receipts() {
        let itx = InternalTransaction::new(TransactionType::PeerAdd, fresh_peer());
        let ok = itx.as_accepted();
        let no = itx.as_refused();
        assert!(ok.accepted);
        assert!(!no.accepted);
        assert_eq!(ok.internal_transaction, itx);
        assert_eq!(no.internal_transaction, itx);
    }
}
