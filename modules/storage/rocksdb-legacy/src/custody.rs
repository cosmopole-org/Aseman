//! ADR 0019: legacy custodial private keys are verified against their creature and never
//! exported. No function here returns, logs, or formats key material.

use super::*;
use rsa::RsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;

/// `true` for the legacy link family that holds plaintext custodial private keys.
#[must_use]
pub fn is_legacy_custodial_key_family(family: &str) -> bool {
    family == "UserPrivateKey"
}

impl LegacySnapshotGraph {
    /// Verify every custodial key and return how many were verified (ADR 0019).
    pub fn verify_legacy_custodial_keys(&self) -> LegacyMigrationResult<usize> {
        let mut verified = 0;
        for (key, value) in &self.links {
            let Some(creature_id) = key.strip_prefix("UserPrivateKey::") else {
                continue;
            };
            let columns = self.object("Creature", creature_id)?;
            let registered = encode_legacy_rsa_public_key(&required_utf8_column(
                "Creature",
                columns,
                "publicKey",
            )?)?;
            let derived = std::str::from_utf8(value)
                .ok()
                .and_then(|pem| RsaPrivateKey::from_pkcs8_pem(pem).ok())
                .and_then(|private| {
                    RsaPublicKey::from(&private)
                        .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
                        .ok()
                })
                .ok_or_else(|| {
                    LegacyMigrationError::Invalid(format!(
                        "legacy custodial key for {creature_id} is not a PKCS#8 RSA private key"
                    ))
                })?;
            if encode_legacy_rsa_public_key(&derived)? != registered {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy custodial key for {creature_id} does not match its registered public key"
                )));
            }
            verified += 1;
        }
        Ok(verified)
    }
}
