//! Translation of `chain/crypto/keys/signature.go`.
//!
//! Signatures are represented, as in Go, by their `(r, s)` integer pair. The
//! `data` passed to [`sign`]/[`verify`] is already a digest (Babble hashes
//! events with SHA256 before signing), so the prehash ECDSA API is used.

use anyhow::{Result, anyhow};
use k256::ecdsa::signature::hazmat::{PrehashSigner, PrehashVerifier};
use k256::ecdsa::{Signature, SigningKey, VerifyingKey};
use num_bigint::BigUint;

/// Signs the digest `data` with the private key.
pub fn sign(priv_key: &SigningKey, data: &[u8]) -> Result<(BigUint, BigUint)> {
    let sig: Signature = priv_key
        .sign_prehash(data)
        .map_err(|e| anyhow!("sign failed: {}", e))?;
    let bytes = sig.to_bytes();
    let r = BigUint::from_bytes_be(&bytes[..32]);
    let s = BigUint::from_bytes_be(&bytes[32..]);
    Ok((r, s))
}

/// Verifies that `(r, s)` is a valid signature of the digest `data` by the
/// owner of the private key associated with `pub_key`.
pub fn verify(pub_key: &VerifyingKey, data: &[u8], r: &BigUint, s: &BigUint) -> bool {
    let mut sig_bytes = [0u8; 64];
    sig_bytes[..32].copy_from_slice(&to_32_be(r));
    sig_bytes[32..].copy_from_slice(&to_32_be(s));
    match Signature::from_slice(&sig_bytes) {
        Ok(sig) => pub_key.verify_prehash(data, &sig).is_ok(),
        Err(_) => false,
    }
}

/// Returns a string representation of a signature: `r|s`, both base-36.
pub fn encode_signature(r: &BigUint, s: &BigUint) -> String {
    format!("{}|{}", r.to_str_radix(36), s.to_str_radix(36))
}

/// Parses a string representation of a signature produced by
/// [`encode_signature`].
pub fn decode_signature(sig: &str) -> Result<(BigUint, BigUint)> {
    let values: Vec<&str> = sig.split('|').collect();
    if values.len() != 2 {
        return Err(anyhow!(
            "wrong number of values in signature: got {}, want 2",
            values.len()
        ));
    }
    let r = BigUint::parse_bytes(values[0].as_bytes(), 36)
        .ok_or_else(|| anyhow!("invalid r value in signature"))?;
    let s = BigUint::parse_bytes(values[1].as_bytes(), 36)
        .ok_or_else(|| anyhow!("invalid s value in signature"))?;
    Ok((r, s))
}

/// Encodes a `BigUint` as a fixed 32-byte big-endian array.
fn to_32_be(n: &BigUint) -> [u8; 32] {
    let bytes = n.to_bytes_be();
    let mut out = [0u8; 32];
    if bytes.len() >= 32 {
        out.copy_from_slice(&bytes[bytes.len() - 32..]);
    } else {
        out[32 - bytes.len()..].copy_from_slice(&bytes);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hash::sha256;
    use crate::crypto::keys::private_key::generate_ecdsa_key;

    // Translation of keys_test.go::TestSignatureEncoding.
    #[test]
    fn test_signature_encoding() {
        let priv_key = generate_ecdsa_key().unwrap();

        let msg = "J'aime mieux forger mon ame que la meubler";
        let msg_hash_bytes = sha256(msg.as_bytes());

        let (r, s) = sign(&priv_key, &msg_hash_bytes).unwrap();
        let encoded_sig = encode_signature(&r, &s);
        let (dr, ds) = decode_signature(&encoded_sig).expect("decode signature");

        assert_eq!(r, dr, "Signature Rs differ");
        assert_eq!(s, ds, "Signature Ss differ");

        // The signature must verify against the matching public key.
        assert!(verify(priv_key.verifying_key(), &msg_hash_bytes, &r, &s));
    }

    #[test]
    fn verify_rejects_wrong_message() {
        let priv_key = generate_ecdsa_key().unwrap();
        let h_a = sha256(b"alpha");
        let h_b = sha256(b"beta");
        let (r, s) = sign(&priv_key, &h_a).unwrap();
        assert!(!verify(priv_key.verifying_key(), &h_b, &r, &s));
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let priv_a = generate_ecdsa_key().unwrap();
        let priv_b = generate_ecdsa_key().unwrap();
        let h = sha256(b"payload");
        let (r, s) = sign(&priv_a, &h).unwrap();
        assert!(!verify(priv_b.verifying_key(), &h, &r, &s));
    }

    #[test]
    fn decode_signature_requires_pipe_separator() {
        assert!(decode_signature("nopipehere").is_err());
        assert!(decode_signature("a|b|c").is_err());
    }

    #[test]
    fn decode_signature_rejects_invalid_base36() {
        assert!(decode_signature("zzz!|abc").is_err());
        assert!(decode_signature("abc|zzz!").is_err());
    }

    #[test]
    fn encode_signature_uses_base36_with_pipe() {
        // 35 in base 36 is "z"; the round-trip should preserve both parts.
        let r = num_bigint::BigUint::from(35u32);
        let s = num_bigint::BigUint::from(36u32);
        let encoded = encode_signature(&r, &s);
        assert_eq!(encoded, "z|10");
        let (dr, ds) = decode_signature(&encoded).unwrap();
        assert_eq!(dr, r);
        assert_eq!(ds, s);
    }
}
