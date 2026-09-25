//! Translation of `chain/node/validator.go`.

use k256::ecdsa::SigningKey;

use crate::crypto::keys;

/// The peer that operates a node, with control over its private key.
///
/// The Go struct cached `id`/`pubBytes`/`pubHex`; the translation derives them
/// on demand, which keeps `Validator` cloneable.
#[derive(Clone)]
pub struct Validator {
    pub key: SigningKey,
    pub moniker: String,
}

impl Validator {
    /// Creates a new `Validator` from a private key and moniker.
    pub fn new(key: SigningKey, moniker: &str) -> Validator {
        Validator {
            key,
            moniker: moniker.to_string(),
        }
    }

    /// The validator's unique numeric ID, derived from its key.
    pub fn id(&self) -> u32 {
        keys::public_key_id(&self.public_key_bytes())
    }

    /// The validator's public key as a byte slice.
    pub fn public_key_bytes(&self) -> Vec<u8> {
        keys::from_public_key(self.key.verifying_key())
    }

    /// The validator's public key as a hex string.
    pub fn public_key_hex(&self) -> String {
        keys::public_key_hex(self.key.verifying_key())
    }
}
