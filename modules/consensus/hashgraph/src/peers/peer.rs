//! Translation of `chain/peers/peer.go`.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::common;
use crate::crypto::keys;

/// Holds Peer data.
///
/// The Go struct cached its `id` in an unexported field; the translation
/// computes [`Peer::id`] on demand instead, which keeps `Peer` cheaply
/// cloneable and thread-safe. The cache was invisible to all callers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Peer {
    /// The IP:PORT of the Babble node. Not necessary with WebRTC.
    pub net_addr: String,
    /// The hexadecimal representation of the peer's public key.
    pub pub_key_hex: String,
    /// An optional, non-unique friendly name for the peer.
    pub moniker: String,
}

impl Peer {
    /// Instantiates a Peer.
    pub fn new(pub_key_hex: &str, net_addr: &str, moniker: &str) -> Peer {
        Peer {
            pub_key_hex: pub_key_hex.to_string(),
            net_addr: net_addr.to_string(),
            moniker: moniker.to_string(),
        }
    }

    /// An ID for the peer, calculated from the public key.
    pub fn id(&self) -> u32 {
        keys::public_key_id(&self.pub_key_bytes())
    }

    /// The upper-case version of `pub_key_hex`, used for indexing in maps.
    pub fn pub_key_string(&self) -> String {
        self.pub_key_hex.to_uppercase()
    }

    /// The byte slice representation of the Peer's public key.
    pub fn pub_key_bytes(&self) -> Vec<u8> {
        common::decode_from_string(&self.pub_key_hex).unwrap_or_default()
    }

    /// Marshals the Peer object. As in Go, the (computed) id is excluded.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        // Go's `json.Encoder.Encode` appends a trailing newline.
        let mut b = serde_json::to_vec(self)?;
        b.push(b'\n');
        Ok(b)
    }

    /// Unmarshals a byte slice into this Peer object.
    pub fn unmarshal(&mut self, data: &[u8]) -> Result<()> {
        *self = serde_json::from_slice(data)?;
        Ok(())
    }
}

/// Excludes a single peer (by id) from a list of peers. Returns the index of
/// the excluded peer (or -1) and the remaining peers.
pub fn exclude_peer(peers: &[Peer], peer: u32) -> (i64, Vec<Peer>) {
    let mut index: i64 = -1;
    let mut other_peers: Vec<Peer> = Vec::with_capacity(peers.len());
    for (i, p) in peers.iter().enumerate() {
        if p.id() != peer {
            other_peers.push(p.clone());
        } else {
            index = i as i64;
        }
    }
    (index, other_peers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::keys::{generate_ecdsa_key, public_key_hex};

    fn fresh_peer(addr: &str, moniker: &str) -> Peer {
        let key = generate_ecdsa_key().unwrap();
        Peer::new(&public_key_hex(key.verifying_key()), addr, moniker)
    }

    #[test]
    fn pub_key_string_uppercases() {
        let p = Peer::new("0xabcdef", "addr", "name");
        assert_eq!(p.pub_key_string(), "0XABCDEF");
    }

    #[test]
    fn id_is_deterministic_for_same_pub_key() {
        let p1 = Peer::new("0XAABB", "addrA", "a");
        let p2 = Peer::new("0XAABB", "addrB", "b");
        // ID is derived only from the public key.
        assert_eq!(p1.id(), p2.id());
    }

    #[test]
    fn id_differs_for_different_pub_keys() {
        let a = fresh_peer("addr1", "m1");
        let b = fresh_peer("addr2", "m2");
        assert_ne!(
            a.id(),
            b.id(),
            "different pub keys should yield different ids"
        );
    }

    #[test]
    fn marshal_unmarshal_round_trip() {
        let p = Peer::new("0X1234", "127.0.0.1:9000", "alice");
        let bytes = p.marshal().expect("marshal");
        // Go's encoder appends a trailing newline; we mirror that.
        assert_eq!(*bytes.last().unwrap(), b'\n');

        let mut out = Peer::default();
        out.unmarshal(&bytes).expect("unmarshal");
        assert_eq!(out, p);
    }

    #[test]
    fn marshal_uses_pascal_case_keys() {
        let p = Peer::new("0X11", "addr", "m");
        let s = String::from_utf8(p.marshal().unwrap()).unwrap();
        assert!(s.contains(r#""NetAddr":"addr""#), "missing NetAddr: {}", s);
        assert!(
            s.contains(r#""PubKeyHex":"0X11""#),
            "missing PubKeyHex: {}",
            s
        );
        assert!(s.contains(r#""Moniker":"m""#), "missing Moniker: {}", s);
    }

    #[test]
    fn pub_key_bytes_returns_empty_for_invalid_hex() {
        let p = Peer::new("0XZZZZ", "addr", "m");
        assert!(p.pub_key_bytes().is_empty());
    }

    #[test]
    fn exclude_peer_removes_match_and_reports_index() {
        let a = fresh_peer("a", "a");
        let b = fresh_peer("b", "b");
        let c = fresh_peer("c", "c");
        let peers = vec![a.clone(), b.clone(), c.clone()];

        let (idx, rest) = exclude_peer(&peers, b.id());
        assert_eq!(idx, 1);
        assert_eq!(rest.len(), 2);
        assert_eq!(rest[0].pub_key_hex, a.pub_key_hex);
        assert_eq!(rest[1].pub_key_hex, c.pub_key_hex);
    }

    #[test]
    fn exclude_peer_returns_minus_one_when_not_found() {
        let a = fresh_peer("a", "a");
        let b = fresh_peer("b", "b");
        let (idx, rest) = exclude_peer(&[a.clone(), b.clone()], 0xDEAD_BEEF);
        assert_eq!(idx, -1);
        assert_eq!(rest.len(), 2);
    }

    #[test]
    fn exclude_peer_on_empty_returns_empty() {
        let (idx, rest) = exclude_peer(&[], 0);
        assert_eq!(idx, -1);
        assert!(rest.is_empty());
    }
}
