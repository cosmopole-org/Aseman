//! The native [`IdentityVerifier`]: A401 proofs checked with the reference codec in
//! `aseman-contracts::identity` (Ed25519, and legacy RSA-PSS for epoch-0 keys).
#![forbid(unsafe_code)]

use aseman_contracts::identity::{
    PublicKey, body_digest, introduction_bytes, verify_proof_signature,
};
use aseman_domain::identity::{
    AuthenticationError, IdentityKey, Introduction, KeyDescription, Proof,
};
use aseman_ports::IdentityVerifier;

/// Verifies proofs in process.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeIdentityVerifier;

impl IdentityVerifier for NativeIdentityVerifier {
    fn verify(&self, proof: &Proof, key: &IdentityKey) -> Result<(), AuthenticationError> {
        let public_key = PublicKey::decode(&key.public_key)?;
        // The directory's key ID must be the key's own (A401 section 2).
        if public_key.key_id() != key.key_id {
            return Err(AuthenticationError::Malformed);
        }
        verify_proof_signature(proof, &public_key)
    }

    fn body_digest(&self, body: &[u8]) -> [u8; 32] {
        body_digest(body)
    }

    fn describe_key(&self, public_key: &[u8]) -> Result<KeyDescription, AuthenticationError> {
        let key = PublicKey::decode(public_key)?;
        Ok(KeyDescription {
            key_id: key.key_id(),
            legacy: key.algorithm().is_legacy(),
        })
    }

    fn introduction_bytes(&self, introduction: &Introduction) -> Vec<u8> {
        introduction_bytes(introduction)
    }
}

#[cfg(test)]
mod tests;
