//! Translation of `chain/hashgraph/block.go`.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use anyhow::{Result, anyhow};
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};

use super::frame::Frame;
use super::internal_transaction::{InternalTransaction, InternalTransactionReceipt};
use crate::common;
use crate::crypto;
use crate::crypto::keys;
use crate::peers::{Peer, PeerSet};
use crate::util::clone_once_lock;

/// The content of a [`Block`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct BlockBody {
    /// Block index.
    pub index: i64,
    /// Round received of the corresponding hashgraph frame.
    pub round_received: i64,
    /// Unix timestamp (median of timestamps in round-received famous witnesses).
    pub timestamp: i64,
    /// Root hash of the application after applying the block payload.
    #[serde(with = "crate::util::bytes_base64")]
    pub state_hash: Vec<u8>,
    /// Hash of the corresponding hashgraph frame.
    #[serde(with = "crate::util::bytes_base64")]
    pub frame_hash: Vec<u8>,
    /// Hash of the peer-set.
    #[serde(with = "crate::util::bytes_base64")]
    pub peers_hash: Vec<u8>,
    /// Transaction payload.
    #[serde(with = "crate::util::bytes_base64_vec")]
    pub transactions: Vec<Vec<u8>>,
    /// Internal transaction payload (add/remove peers).
    pub internal_transactions: Vec<InternalTransaction>,
    /// Receipts for internal transactions; populated by application Commit.
    pub internal_transaction_receipts: Vec<InternalTransactionReceipt>,
}

impl BlockBody {
    /// Produces the JSON encoding of a `BlockBody`.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        let mut b = serde_json::to_vec(self)?;
        b.push(b'\n');
        Ok(b)
    }

    /// Parses a JSON encoded `BlockBody`.
    pub fn unmarshal(&mut self, data: &[u8]) -> Result<()> {
        *self = serde_json::from_slice(data)?;
        Ok(())
    }

    /// Produces the SHA256 hash of the marshalled `BlockBody`.
    pub fn hash(&self) -> Result<Vec<u8>> {
        Ok(crypto::sha256(&self.marshal()?))
    }
}

/// A signature encoded as a string, with the block index and the signer's
/// public key.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct BlockSignature {
    /// The public key of the signer.
    #[serde(with = "crate::util::bytes_base64")]
    pub validator: Vec<u8>,
    /// Block index.
    pub index: i64,
    /// String encoding of the signature.
    pub signature: String,
}

impl BlockSignature {
    /// Returns the hex string representation of the signer's public key.
    pub fn validator_hex(&self) -> String {
        common::encode_to_string(&self.validator)
    }

    /// Produces the JSON encoding of the `BlockSignature`.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        let mut b = serde_json::to_vec(self)?;
        b.push(b'\n');
        Ok(b)
    }

    /// Parses a `BlockSignature` from JSON.
    pub fn unmarshal(&mut self, data: &[u8]) -> Result<()> {
        *self = serde_json::from_slice(data)?;
        Ok(())
    }

    /// Returns the wire representation of a `BlockSignature`.
    pub fn to_wire(&self) -> WireBlockSignature {
        WireBlockSignature {
            index: self.index,
            signature: self.signature.clone(),
        }
    }

    /// Produces a string identifier of the signature for key-value storage.
    pub fn key(&self) -> String {
        format!("{}-{}", self.index, self.validator_hex())
    }
}

/// A light-weight representation of a signature travelling over the wire.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct WireBlockSignature {
    pub index: i64,
    pub signature: String,
}

/// A section of the Hashgraph that has reached consensus.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Block {
    pub body: BlockBody,
    /// `[validator hex] => signature`.
    pub signatures: BTreeMap<String, String>,

    #[serde(skip)]
    hash: OnceLock<Vec<u8>>,
    #[serde(skip)]
    hex: OnceLock<String>,
    #[serde(skip)]
    peer_set: Option<Arc<PeerSet>>,
}

impl Clone for Block {
    fn clone(&self) -> Self {
        Block {
            body: self.body.clone(),
            signatures: self.signatures.clone(),
            hash: clone_once_lock(&self.hash),
            hex: clone_once_lock(&self.hex),
            peer_set: self.peer_set.clone(),
        }
    }
}

impl Default for Block {
    fn default() -> Self {
        Block {
            body: BlockBody::default(),
            signatures: BTreeMap::new(),
            hash: OnceLock::new(),
            hex: OnceLock::new(),
            peer_set: None,
        }
    }
}

impl Block {
    /// Assembles a block from a [`Frame`].
    pub fn new_from_frame(block_index: i64, frame: &Frame) -> Result<Block> {
        let frame_hash = frame.hash()?;

        let mut transactions: Vec<Vec<u8>> = Vec::new();
        let mut internal_transactions: Vec<InternalTransaction> = Vec::new();
        for e in &frame.events {
            transactions.extend(e.core.transactions().iter().cloned());
            internal_transactions.extend(e.core.internal_transactions().iter().cloned());
        }

        Ok(Block::new(
            block_index,
            frame.round,
            frame_hash,
            frame.peers.clone(),
            transactions,
            internal_transactions,
            frame.timestamp,
        ))
    }

    /// Creates a new `Block`.
    pub fn new(
        block_index: i64,
        round_received: i64,
        frame_hash: Vec<u8>,
        peer_slice: Vec<Peer>,
        txs: Vec<Vec<u8>>,
        itxs: Vec<InternalTransaction>,
        timestamp: i64,
    ) -> Block {
        let peer_set = PeerSet::new(peer_slice);
        let peers_hash = peer_set.hash();

        let body = BlockBody {
            index: block_index,
            round_received,
            state_hash: Vec::new(),
            frame_hash,
            peers_hash,
            transactions: txs,
            internal_transactions: itxs,
            timestamp,
            internal_transaction_receipts: Vec::new(),
        };

        Block {
            body,
            signatures: BTreeMap::new(),
            hash: OnceLock::new(),
            hex: OnceLock::new(),
            peer_set: Some(Arc::new(peer_set)),
        }
    }

    pub fn index(&self) -> i64 {
        self.body.index
    }
    pub fn timestamp(&self) -> i64 {
        self.body.timestamp
    }
    pub fn transactions(&self) -> &[Vec<u8>] {
        &self.body.transactions
    }
    pub fn internal_transactions(&self) -> &[InternalTransaction] {
        &self.body.internal_transactions
    }
    pub fn internal_transaction_receipts(&self) -> &[InternalTransactionReceipt] {
        &self.body.internal_transaction_receipts
    }
    pub fn round_received(&self) -> i64 {
        self.body.round_received
    }
    pub fn state_hash(&self) -> &[u8] {
        &self.body.state_hash
    }
    pub fn frame_hash(&self) -> &[u8] {
        &self.body.frame_hash
    }
    pub fn peers_hash(&self) -> &[u8] {
        &self.body.peers_hash
    }

    /// Returns the block's signatures.
    pub fn get_signatures(&self) -> Vec<BlockSignature> {
        self.signatures
            .iter()
            .map(|(val, sig)| {
                let validator = common::decode_from_string(val).unwrap_or_default();
                BlockSignature {
                    validator,
                    index: self.index(),
                    signature: sig.clone(),
                }
            })
            .collect()
    }

    /// Returns a block signature from a specific validator.
    pub fn get_signature(&self, validator: &str) -> Result<BlockSignature> {
        let sig = self
            .signatures
            .get(validator)
            .ok_or_else(|| anyhow!("signature not found"))?;
        let validator_bytes = common::decode_from_string(validator).unwrap_or_default();
        Ok(BlockSignature {
            validator: validator_bytes,
            index: self.index(),
            signature: sig.clone(),
        })
    }

    /// Adds transactions to the block.
    pub fn append_transactions(&mut self, txs: Vec<Vec<u8>>) {
        self.body.transactions.extend(txs);
    }

    /// Produces a JSON encoding of the Block.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        let mut b = serde_json::to_vec(self)?;
        b.push(b'\n');
        Ok(b)
    }

    /// Parses a JSON encoded Block.
    pub fn unmarshal(&mut self, data: &[u8]) -> Result<()> {
        let decoded: Block = serde_json::from_slice(data)?;
        self.body = decoded.body;
        self.signatures = decoded.signatures;
        self.hash = OnceLock::new();
        self.hex = OnceLock::new();
        Ok(())
    }

    /// Returns the SHA256 encoding of a marshalled block.
    pub fn hash(&self) -> Result<Vec<u8>> {
        if let Some(h) = self.hash.get() {
            return Ok(h.clone());
        }
        let h = crypto::sha256(&self.marshal()?);
        let _ = self.hash.set(h.clone());
        Ok(h)
    }

    /// Returns the hex string representation of the block's hash.
    pub fn hex(&self) -> String {
        if let Some(h) = self.hex.get() {
            return h.clone();
        }
        let hash = self.hash().unwrap_or_default();
        let hex = common::encode_to_string(&hash);
        let _ = self.hex.set(hex.clone());
        hex
    }

    /// Returns the signature of the hash of the block's body.
    pub fn sign(&self, priv_key: &SigningKey) -> Result<BlockSignature> {
        let sign_bytes = self.body.hash()?;
        let (r, s) = keys::sign(priv_key, &sign_bytes)?;
        Ok(BlockSignature {
            validator: keys::from_public_key(priv_key.verifying_key()),
            index: self.index(),
            signature: keys::encode_signature(&r, &s),
        })
    }

    /// Appends a signature to the block.
    pub fn set_signature(&mut self, bs: BlockSignature) -> Result<()> {
        self.signatures.insert(bs.validator_hex(), bs.signature);
        Ok(())
    }

    /// Verifies that a signature is valid against the block.
    pub fn verify(&self, sig: BlockSignature) -> Result<bool> {
        let sign_bytes = self.body.hash()?;
        let pub_key = match keys::to_public_key(&sig.validator) {
            Some(pk) => pk,
            None => return Ok(false),
        };
        let (r, s) = keys::decode_signature(&sig.signature)?;
        Ok(keys::verify(&pub_key, &sign_bytes, &r, &s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::keys::{generate_ecdsa_key, public_key_hex};
    use crate::hashgraph::internal_transaction::{InternalTransaction, TransactionType};
    use crate::peers::Peer;

    fn create_test_block() -> Block {
        let mut block = Block::new(
            0,
            1,
            b"framehash".to_vec(),
            vec![],
            vec![b"abc".to_vec(), b"def".to_vec(), b"ghi".to_vec()],
            vec![
                InternalTransaction::new(
                    TransactionType::PeerAdd,
                    Peer::new("peer1", "paris", "peer1"),
                ),
                InternalTransaction::new(
                    TransactionType::PeerRemove,
                    Peer::new("peer2", "london", "peer2"),
                ),
            ],
            0,
        );

        let receipts: Vec<InternalTransactionReceipt> = block
            .internal_transactions()
            .iter()
            .map(|itx| itx.as_accepted())
            .collect();
        block.body.internal_transaction_receipts = receipts;
        block
    }

    // Translation of block_test.go::TestSignBlock.
    #[test]
    fn test_sign_block() {
        let private_key = generate_ecdsa_key().unwrap();
        let block = create_test_block();
        let sig = block.sign(&private_key).expect("sign");
        let res = block.verify(sig).expect("verify");
        assert!(res, "Verify returned false");
    }

    // Translation of block_test.go::TestAppendSignature.
    #[test]
    fn test_append_signature() {
        let private_key = generate_ecdsa_key().unwrap();
        let mut block = create_test_block();
        let sig = block.sign(&private_key).expect("sign");
        block.set_signature(sig).expect("set signature");

        let block_signature = block
            .get_signature(&public_key_hex(private_key.verifying_key()))
            .expect("get signature");
        let res = block.verify(block_signature).expect("verify");
        assert!(res, "Verify returned false");
    }

    // Translation of block_test.go::TestNewBlockFromFrame.
    #[test]
    fn test_new_block_from_frame() {
        use crate::hashgraph::event::{Event, EventBody, FrameEvent};
        use crate::hashgraph::frame::Frame;

        let frame_timestamp: i64 = 123456789;

        let transactions: Vec<Vec<u8>> = (1..=9)
            .map(|i| format!("transaction{}", i).into_bytes())
            .collect();

        let internal_transactions = vec![
            InternalTransaction::new(
                TransactionType::PeerAdd,
                Peer::new("peer1000.pub", "peer1000.addr", "peer1000"),
            ),
            InternalTransaction::new(
                TransactionType::PeerAdd,
                Peer::new("peer1001.pub", "peer1001.addr", "peer1001"),
            ),
            InternalTransaction::new(
                TransactionType::PeerAdd,
                Peer::new("peer1002.pub", "peer1002.addr", "peer1002"),
            ),
        ];

        let mk_event = |txs: &[Vec<u8>], itxs: &[InternalTransaction]| FrameEvent {
            core: Box::new(Event {
                body: EventBody {
                    transactions: txs.to_vec(),
                    internal_transactions: itxs.to_vec(),
                    ..EventBody::default()
                },
                ..Event::default()
            }),
            ..FrameEvent::default()
        };

        let frame = Frame {
            round: 56,
            peers: vec![
                Peer::new("peer1.pub", "peer1.addr", "peer1"),
                Peer::new("peer2.pub", "peer2.addr", "peer2"),
                Peer::new("peer3.pub", "peer3.addr", "peer3"),
            ],
            events: vec![
                mk_event(&transactions[0..3], &internal_transactions[..1]),
                mk_event(&transactions[3..6], &internal_transactions[1..2]),
                mk_event(&transactions[6..], &internal_transactions[2..]),
            ],
            timestamp: frame_timestamp,
            ..Frame::default()
        };

        let block = Block::new_from_frame(10, &frame).expect("new block from frame");

        assert_eq!(block.index(), 10, "block index");
        assert_eq!(block.round_received(), frame.round, "block round received");

        let frame_hash = frame.hash().unwrap();
        assert_eq!(
            block.frame_hash(),
            frame_hash.as_slice(),
            "block frame hash"
        );
        assert_eq!(
            block.transactions(),
            transactions.as_slice(),
            "block transactions"
        );
        assert_eq!(
            block.internal_transactions(),
            internal_transactions.as_slice(),
            "block internal transactions"
        );
        assert_eq!(block.timestamp(), frame_timestamp, "block timestamp");
    }
}
