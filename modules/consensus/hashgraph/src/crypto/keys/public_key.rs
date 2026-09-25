//! Translation of `chain/crypto/keys/public_key.go`.

use k256::ecdsa::VerifyingKey;

use crate::common;

/// Parses the uncompressed form of a public key (as produced by
/// [`from_public_key`]). Returns `None` for an empty or invalid input, matching
/// Go's `elliptic.Unmarshal` returning nil coordinates.
pub fn to_public_key(pub_bytes: &[u8]) -> Option<VerifyingKey> {
    if pub_bytes.is_empty() {
        return None;
    }
    VerifyingKey::from_sec1_bytes(pub_bytes).ok()
}

/// Outputs the public key in uncompressed form (65 bytes: `0x04 || X || Y`).
pub fn from_public_key(pub_key: &VerifyingKey) -> Vec<u8> {
    pub_key.to_encoded_point(false).as_bytes().to_vec()
}

/// Tries to give a unique `u32` representation of the public key. Used to save
/// space in the wire encoding of hashgraph Events.
pub fn public_key_id(pub_bytes: &[u8]) -> u32 {
    hash32(pub_bytes)
}

/// The 32-bit FNV-1a hash of `data`.
fn hash32(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5; // FNV offset basis
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193); // FNV prime
    }
    h
}

/// Returns the hexadecimal representation of the uncompressed public key.
pub fn public_key_hex(pub_key: &VerifyingKey) -> String {
    common::encode_to_string(&from_public_key(pub_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::keys::private_key::generate_ecdsa_key;

    #[test]
    fn to_public_key_round_trip_through_from_public_key() {
        let priv_key = generate_ecdsa_key().unwrap();
        let bytes = from_public_key(priv_key.verifying_key());
        assert_eq!(bytes.len(), 65, "uncompressed sec1 key is 65 bytes");
        assert_eq!(bytes[0], 0x04, "uncompressed marker is 0x04");

        let parsed = to_public_key(&bytes).expect("parse round-trip");
        assert_eq!(from_public_key(&parsed), bytes);
    }

    #[test]
    fn to_public_key_rejects_empty_and_garbage() {
        assert!(to_public_key(&[]).is_none());
        assert!(to_public_key(&[0x00, 0x01, 0x02]).is_none());
    }

    #[test]
    fn public_key_id_is_deterministic_and_nonzero_for_typical_keys() {
        let pk = generate_ecdsa_key().unwrap();
        let bytes = from_public_key(pk.verifying_key());
        let id1 = public_key_id(&bytes);
        let id2 = public_key_id(&bytes);
        assert_eq!(id1, id2);
        // FNV-1a of a 65-byte input is overwhelmingly unlikely to collide on 0.
        assert_ne!(id1, 0);
    }

    #[test]
    fn public_key_id_matches_known_fnv1a_vectors() {
        // FNV-1a("") = 0x811c9dc5 (offset basis).
        assert_eq!(public_key_id(b""), 0x811c_9dc5);
        // FNV-1a("a") = 0xe40c292c.
        assert_eq!(public_key_id(b"a"), 0xe40c_292c);
        // FNV-1a("foobar") = 0xbf9cf968.
        assert_eq!(public_key_id(b"foobar"), 0xbf9c_f968);
    }

    #[test]
    fn public_key_hex_has_0x_prefix_and_is_uppercase() {
        let pk = generate_ecdsa_key().unwrap();
        let hex = public_key_hex(pk.verifying_key());
        assert!(hex.starts_with("0X"));
        assert_eq!(hex, hex.to_uppercase());
        // Length: "0X" + 2 hex chars per byte * 65 bytes = 132.
        assert_eq!(hex.len(), 2 + 130);
    }

    #[test]
    fn different_keys_produce_different_ids() {
        let a = generate_ecdsa_key().unwrap();
        let b = generate_ecdsa_key().unwrap();
        let id_a = public_key_id(&from_public_key(a.verifying_key()));
        let id_b = public_key_id(&from_public_key(b.verifying_key()));
        assert_ne!(id_a, id_b);
    }
}
