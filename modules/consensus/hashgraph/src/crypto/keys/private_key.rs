//! Translation of `chain/crypto/keys/private_key.go`.
//!
//! Go represented keys as `*ecdsa.PrivateKey`; this translation uses
//! `k256::ecdsa::SigningKey`. The `paddedBigBytes`/`readBits` helpers from the
//! Go file produced a fixed-width big-endian encoding of the key's D value —
//! `SigningKey::to_bytes()` already returns exactly that 32-byte form, so those
//! private helpers have no separate translation.

use anyhow::{Result, anyhow};
use k256::FieldBytes;
use k256::ecdsa::SigningKey;

/// Creates a new secp256k1 `SigningKey`.
///
/// Mirrors `ecdsa.GenerateKey`: random 32-byte scalars are drawn until one is a
/// valid private key (non-zero and below the curve order N).
pub fn generate_ecdsa_key() -> Result<SigningKey> {
    loop {
        let mut d = [0u8; 32];
        getrandom::getrandom(&mut d).map_err(|e| anyhow!("getrandom failed: {}", e))?;
        if let Ok(key) = SigningKey::from_bytes(FieldBytes::from_slice(&d)) {
            return Ok(key);
        }
    }
}

/// Exports a private key into a binary dump (32-byte big-endian D value).
pub fn dump_private_key(priv_key: &SigningKey) -> Vec<u8> {
    priv_key.to_bytes().to_vec()
}

/// Creates a private key with the given D value.
pub fn parse_private_key(d: &[u8]) -> Result<SigningKey> {
    if 8 * d.len() != 256 {
        return Err(anyhow!("invalid length, need 256 bits"));
    }
    SigningKey::from_bytes(FieldBytes::from_slice(d)).map_err(|_| {
        // Distinguish the two validation failures Go reported explicitly.
        anyhow!("invalid private key, zero or >=N")
    })
}

/// Returns the hexadecimal representation of a raw private key.
pub fn private_key_hex(key: &SigningKey) -> String {
    hex::encode(dump_private_key(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_returns_keys_of_expected_layout() {
        let k = generate_ecdsa_key().expect("generate");
        let d = dump_private_key(&k);
        assert_eq!(d.len(), 32, "secp256k1 D value is 32 bytes");
    }

    #[test]
    fn generate_yields_distinct_keys() {
        let a = generate_ecdsa_key().unwrap();
        let b = generate_ecdsa_key().unwrap();
        assert_ne!(dump_private_key(&a), dump_private_key(&b));
    }

    #[test]
    fn dump_and_parse_round_trip() {
        let original = generate_ecdsa_key().unwrap();
        let bytes = dump_private_key(&original);
        let parsed = parse_private_key(&bytes).expect("parse");
        assert_eq!(dump_private_key(&parsed), bytes);
    }

    #[test]
    fn parse_rejects_wrong_length() {
        let err = parse_private_key(&[0u8; 16]).expect_err("too short");
        assert!(format!("{}", err).contains("256 bits"));
        let err = parse_private_key(&[0u8; 64]).expect_err("too long");
        assert!(format!("{}", err).contains("256 bits"));
    }

    #[test]
    fn parse_rejects_zero_scalar() {
        // Zero is an invalid scalar for ECDSA.
        let err = parse_private_key(&[0u8; 32]).expect_err("zero scalar");
        assert!(format!("{}", err).contains("invalid private key"));
    }

    #[test]
    fn private_key_hex_is_64_hex_chars() {
        let k = generate_ecdsa_key().unwrap();
        let h = private_key_hex(&k);
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
