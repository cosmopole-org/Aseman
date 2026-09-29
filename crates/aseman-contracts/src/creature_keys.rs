//! Canonical encoding of legacy creature public keys (A304 `public_key` bytes).
//!
//! Legacy stores the client's RSA SPKI PEM string. Capsules store the multicodec
//! `rsa-pub` (0x1205) prefix followed by the SPKI DER, so two PEM spellings of one key
//! are one value. Decoding yields the standard PEM spelling of that DER.

use rsa::RsaPublicKey;
use rsa::pkcs8::{DecodePublicKey, EncodePublicKey, LineEnding};
use thiserror::Error;

/// Multicodec `rsa-pub` (0x1205) as an unsigned varint.
const RSA_PUB_MULTICODEC: [u8; 2] = [0x85, 0x24];

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum LegacyKeyError {
    #[error("legacy Creature publicKey is not RSA SPKI PEM")]
    NotRsaPem,
    #[error("legacy Creature RSA key cannot encode as SPKI")]
    Encode,
    #[error("capsule public_key is not a multicodec RSA SPKI key")]
    NotMulticodecRsa,
}

/// Encode a legacy RSA SPKI PEM public key as canonical capsule bytes.
pub fn encode_legacy_rsa_public_key(public_key_pem: &str) -> Result<Vec<u8>, LegacyKeyError> {
    let key =
        RsaPublicKey::from_public_key_pem(public_key_pem).map_err(|_| LegacyKeyError::NotRsaPem)?;
    let der = key
        .to_public_key_der()
        .map_err(|_| LegacyKeyError::Encode)?;
    let mut encoded = Vec::with_capacity(RSA_PUB_MULTICODEC.len() + der.as_bytes().len());
    encoded.extend_from_slice(&RSA_PUB_MULTICODEC);
    encoded.extend_from_slice(der.as_bytes());
    Ok(encoded)
}

/// Decode canonical capsule bytes back to the standard SPKI PEM spelling (LF).
pub fn decode_legacy_rsa_public_key(encoded: &[u8]) -> Result<String, LegacyKeyError> {
    let der = encoded
        .strip_prefix(&RSA_PUB_MULTICODEC)
        .ok_or(LegacyKeyError::NotMulticodecRsa)?;
    RsaPublicKey::from_public_key_der(der)
        .map_err(|_| LegacyKeyError::NotMulticodecRsa)?
        .to_public_key_pem(LineEnding::LF)
        .map_err(|_| LegacyKeyError::Encode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_spellings_share_one_encoding_and_decode_to_standard_pem() {
        let key =
            RsaPublicKey::from(&rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap());
        let standard = key.to_public_key_pem(LineEnding::LF).unwrap();
        let windows = key.to_public_key_pem(LineEnding::CRLF).unwrap();
        let encoded = encode_legacy_rsa_public_key(&standard).unwrap();
        assert_eq!(encoded[..2], RSA_PUB_MULTICODEC);
        assert_eq!(encode_legacy_rsa_public_key(&windows).unwrap(), encoded);
        assert_eq!(decode_legacy_rsa_public_key(&encoded).unwrap(), standard);
        assert_eq!(
            encode_legacy_rsa_public_key("not a key"),
            Err(LegacyKeyError::NotRsaPem)
        );
        assert_eq!(
            decode_legacy_rsa_public_key(&encoded[2..]),
            Err(LegacyKeyError::NotMulticodecRsa)
        );
    }
}
