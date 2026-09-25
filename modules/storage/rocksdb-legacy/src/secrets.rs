//! ADR 0023: legacy creature secrets migrate as authenticated ciphertext (never
//! plaintext); grants migrate with verified reverse links; login grants are dropped.

use super::*;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

pub const LEGACY_SECRET_ALGORITHM: &str = "chacha20poly1305-legacy-v1";
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// The legacy node master key. `Debug` is redacted so key material cannot leak.
#[derive(Clone, Eq, PartialEq)]
pub struct LegacySecretMasterKey([u8; 32]);

impl LegacySecretMasterKey {
    /// Parse the legacy `node-secret-key` file contents (base64 of 32 bytes).
    pub fn from_key_file(contents: &str) -> LegacyMigrationResult<Self> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(contents.trim())
            .map_err(|_| {
                LegacyMigrationError::Invalid("legacy secret key file is not base64".to_owned())
            })?;
        let key = <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
            LegacyMigrationError::Invalid("legacy secret key is not 32 bytes".to_owned())
        })?;
        Ok(Self(key))
    }

    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"ASEMAN-LEGACY-SECRET-KEY-FINGERPRINT-V1\0");
        hasher.update(self.0);
        hasher.finalize().into()
    }

    /// Authenticate a ciphertext; the plaintext is dropped before returning.
    fn authenticates(&self, nonce_and_ciphertext: &[u8]) -> bool {
        if nonce_and_ciphertext.len() < NONCE_LEN + TAG_LEN {
            return false;
        }
        let (nonce, ciphertext) = nonce_and_ciphertext.split_at(NONCE_LEN);
        ChaCha20Poly1305::new(Key::from_slice(&self.0))
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .is_ok()
    }
}

impl std::fmt::Debug for LegacySecretMasterKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LegacySecretMasterKey(<redacted>)")
    }
}

/// `true` for a link family reviewed by ADR 0023.
#[must_use]
pub fn is_legacy_secret_link_family(family: &str) -> bool {
    matches!(
        family,
        "Secret" | "SecretGrant" | "SecretGrantee" | "LoginGrant"
    )
}

impl LegacySnapshotGraph {
    pub(crate) fn transform_legacy_secrets(
        &self,
        migration_time_micros: i64,
        key: Option<&LegacySecretMasterKey>,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let mut capsules = Vec::new();
        let mut secrets = BTreeSet::new();
        let mut grants = BTreeMap::new();
        let mut reverse = BTreeMap::new();
        for (link, value) in &self.links {
            let Some((family, rest)) = link.split_once("::") else {
                continue;
            };
            let text = || {
                String::from_utf8(value.clone()).map_err(|_| {
                    LegacyMigrationError::Invalid(format!("legacy {family} link is not UTF-8"))
                })
            };
            match family {
                "Secret" => {
                    let (owner, name) = rest
                        .split_once("::")
                        .filter(|(owner, name)| {
                            !owner.is_empty() && !name.is_empty() && !name.contains(':')
                        })
                        .ok_or_else(|| {
                            LegacyMigrationError::Invalid("malformed legacy secret key".to_owned())
                        })?;
                    self.object("Creature", owner)?;
                    let key = key.ok_or_else(|| LegacyMigrationError::Unmapped {
                        family: "Secret.master_key".to_owned(),
                        key: format!("{owner}::{name}"),
                    })?;
                    let blob = base64::engine::general_purpose::STANDARD
                        .decode(text()?.trim())
                        .ok()
                        .filter(|blob| key.authenticates(blob))
                        .ok_or_else(|| {
                            LegacyMigrationError::Invalid(format!(
                                "legacy secret {owner}::{name} does not authenticate under the supplied master key"
                            ))
                        })?;
                    secrets.insert((owner.to_owned(), name.to_owned()));
                    capsules.push(seal_legacy_capsule(
                        LegacyCapsuleSpec {
                            family: "CreatureSecret",
                            kind: "core.creature_secret",
                            storage_class: StorageClass::Core,
                            owner_scope: OwnerScope::Creature(deterministic_legacy_capsule_id(
                                "Creature",
                                owner.as_bytes(),
                            )),
                            migration_time_micros,
                        },
                        rest,
                        vec![CapsuleRelationship {
                            name: "creature".to_owned(),
                            target_kind: CapsuleKind("core.creature".to_owned()),
                            target_id: CapsuleId(deterministic_legacy_capsule_id(
                                "Creature",
                                owner.as_bytes(),
                            )),
                        }],
                        BTreeMap::from([
                            ("name".to_owned(), CapsuleValue::Text(name.to_owned())),
                            (
                                "algorithm".to_owned(),
                                CapsuleValue::Text(LEGACY_SECRET_ALGORITHM.to_owned()),
                            ),
                            ("ciphertext".to_owned(), CapsuleValue::Bytes(blob)),
                            (
                                "key_fingerprint".to_owned(),
                                CapsuleValue::Bytes(key.fingerprint().to_vec()),
                            ),
                        ]),
                    )?);
                }
                "SecretGrant" | "SecretGrantee" => {
                    let parts = rest.splitn(3, "::").collect::<Vec<_>>();
                    let [first, second, third] = parts.as_slice() else {
                        return Err(LegacyMigrationError::Invalid(format!(
                            "malformed legacy {family} key"
                        )));
                    };
                    let expires = text()?
                        .trim()
                        .parse::<i64>()
                        .ok()
                        .filter(|expires| *expires > 0)
                        .ok_or_else(|| {
                            LegacyMigrationError::Invalid(format!(
                                "legacy {family} expiry is not positive"
                            ))
                        })?;
                    // Normalize both to (owner, name, grantee).
                    let identity = if family == "SecretGrant" {
                        (
                            (*first).to_owned(),
                            (*second).to_owned(),
                            (*third).to_owned(),
                        )
                    } else {
                        (
                            (*second).to_owned(),
                            (*third).to_owned(),
                            (*first).to_owned(),
                        )
                    };
                    let target = if family == "SecretGrant" {
                        &mut grants
                    } else {
                        &mut reverse
                    };
                    target.insert(identity, expires);
                }
                "LoginGrant" => {
                    // Ephemeral single-use bearer nonce: shape-checked, never migrated.
                    let text = text()?;
                    let valid = text.split_once('|').is_some_and(|(expires, email)| {
                        expires.parse::<i64>().is_ok() && email.contains('@')
                    });
                    if rest.is_empty() || !valid {
                        return Err(LegacyMigrationError::Invalid(
                            "legacy login grant does not match the reviewed shape".to_owned(),
                        ));
                    }
                }
                _ => {}
            }
        }
        if grants != reverse {
            return Err(LegacyMigrationError::Invalid(
                "legacy SecretGrantee reverse links do not mirror the grants exactly".to_owned(),
            ));
        }
        for ((owner, name, grantee), expires) in grants {
            if !secrets.contains(&(owner.clone(), name.clone())) {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy grant for {owner}::{name} names no secret"
                )));
            }
            let expires_micros = expires.checked_mul(1_000).ok_or_else(|| {
                LegacyMigrationError::Invalid(
                    "legacy grant expiry overflows microseconds".to_owned(),
                )
            })?;
            let mut relationships = vec![CapsuleRelationship {
                name: "secret".to_owned(),
                target_kind: CapsuleKind("core.creature_secret".to_owned()),
                target_id: CapsuleId(deterministic_legacy_capsule_id(
                    "CreatureSecret",
                    format!("{owner}::{name}").as_bytes(),
                )),
            }];
            if self
                .objects
                .contains_key(&("Creature".to_owned(), grantee.clone()))
            {
                relationships.push(CapsuleRelationship {
                    name: "grantee".to_owned(),
                    target_kind: CapsuleKind("core.creature".to_owned()),
                    target_id: CapsuleId(deterministic_legacy_capsule_id(
                        "Creature",
                        grantee.as_bytes(),
                    )),
                });
            }
            capsules.push(seal_legacy_capsule(
                LegacyCapsuleSpec {
                    family: "SecretGrant",
                    kind: "core.secret_grant",
                    storage_class: StorageClass::Core,
                    owner_scope: OwnerScope::Creature(deterministic_legacy_capsule_id(
                        "Creature",
                        owner.as_bytes(),
                    )),
                    migration_time_micros,
                },
                &format!("{owner}::{name}::{grantee}"),
                relationships,
                BTreeMap::from([
                    ("grantee_ref".to_owned(), CapsuleValue::Text(grantee)),
                    (
                        "expires_at_micros".to_owned(),
                        CapsuleValue::Integer(expires_micros),
                    ),
                ]),
            )?);
        }
        Ok(capsules)
    }
}
