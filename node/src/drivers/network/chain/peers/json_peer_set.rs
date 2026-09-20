//! Translation of `chain/peers/json_peer_set.go`.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;

use super::peer::Peer;
use super::peer_set::PeerSet;

const JSON_PEER_SET_PATH: &str = "peers.json";
const JSON_GENESIS_PEER_SET_PATH: &str = "peers.genesis.json";

/// Provides peer persistence on disk in the form of a JSON file.
pub struct JSONPeerSet {
    l: Mutex<()>,
    path: PathBuf,
}

impl JSONPeerSet {
    /// Creates a new `JSONPeerSet` referencing a base directory where the JSON
    /// files reside.
    pub fn new(base: &str, is_current: bool) -> JSONPeerSet {
        let path = if is_current {
            PathBuf::from(base).join(JSON_PEER_SET_PATH)
        } else {
            PathBuf::from(base).join(JSON_GENESIS_PEER_SET_PATH)
        };
        JSONPeerSet {
            l: Mutex::new(()),
            path,
        }
    }

    /// Parses the underlying JSON file and returns the corresponding `PeerSet`.
    ///
    /// Returns `Ok(None)` when the file exists but is empty, and `Err` when the
    /// file cannot be read — matching the Go `(nil, nil)` / `(nil, err)` cases.
    pub fn peer_set(&self) -> Result<Option<PeerSet>> {
        let _guard = self.l.lock().unwrap();

        let buf = std::fs::read(&self.path)?;
        if buf.is_empty() {
            return Ok(None);
        }

        let mut peers: Vec<Peer> = serde_json::from_slice(&buf)?;
        cleanse_peer_set(&mut peers);

        Ok(Some(PeerSet::new(peers)))
    }

    /// Persists a PeerSet to a JSON file.
    pub fn write(&self, peers: &[Peer]) -> Result<()> {
        let _guard = self.l.lock().unwrap();

        let mut buf = serde_json::to_vec(peers)?;
        buf.push(b'\n');
        std::fs::write(&self.path, buf)?;
        Ok(())
    }
}

/// Standardises the public key strings to match the format Babble derives from
/// a private key.
fn cleanse_peer_set(peers: &mut [Peer]) {
    for peer in peers.iter_mut() {
        let upper = peer.pub_key_hex.to_uppercase();
        let trimmed = upper.strip_prefix("0X").unwrap_or(&upper);
        peer.pub_key_hex = format!("0X{}", trimmed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::network::chain::crypto::keys;
    use std::collections::HashMap;

    fn temp_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("babble-peers-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Translation of json_peer_set_test.go::TestJSONPeerSet.
    #[test]
    fn test_json_peer_set() {
        let dir = temp_dir();
        let store = JSONPeerSet::new(dir.to_str().unwrap(), true);

        // Try a read, should get an error (no file).
        assert!(store.peer_set().is_err(), "store.peer_set() should error");

        let mut signing_keys: HashMap<String, k256::ecdsa::VerifyingKey> = HashMap::new();
        let mut peers: Vec<Peer> = Vec::new();
        for i in 0..3 {
            let key = keys::generate_ecdsa_key().unwrap();
            let peer = Peer {
                net_addr: format!("addr{}", i),
                pub_key_hex: keys::public_key_hex(key.verifying_key()),
                moniker: format!("peer{}", i),
            };
            signing_keys.insert(peer.net_addr.clone(), *key.verifying_key());
            peers.push(peer);
        }

        let new_peer_set = PeerSet::new(peers.clone());
        let new_peer_slice = &new_peer_set.peers;

        store.write(new_peer_slice).expect("write");

        let peer_set = store.peer_set().expect("read").expect("peer set present");
        assert_eq!(peer_set.len(), 3, "should find 3 peers");

        let peer_slice = &peer_set.peers;
        for i in 0..3usize {
            assert_eq!(peer_slice[i].net_addr, new_peer_slice[i].net_addr);
            assert_eq!(peer_slice[i].moniker, new_peer_slice[i].moniker);
            assert_eq!(peer_slice[i].pub_key_hex, new_peer_slice[i].pub_key_hex);

            let pub_key_bytes = peer_slice[i].pub_key_bytes();
            let pub_key = keys::to_public_key(&pub_key_bytes).expect("parse pub key");
            let original = signing_keys.get(&peer_slice[i].net_addr).unwrap();
            assert_eq!(
                keys::from_public_key(&pub_key),
                keys::from_public_key(original),
                "peers[{}] PublicKey not parsed correctly",
                i
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
