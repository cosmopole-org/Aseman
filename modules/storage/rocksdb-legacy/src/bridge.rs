//! ADR 0024: bridge grants (digest-keyed) and topic claims migrate; `NodeIpToHost`
//! has no reviewed writer and fails closed.

use super::*;

pub const LEGACY_BRIDGE_GRANT_PREFIX: &str = "Json::BridgeGrant::";

/// `true` for a link family ADR 0024 migrates.
#[must_use]
pub fn is_legacy_bridge_link_family(family: &str) -> bool {
    family == "BridgeTopicOwner"
}

impl LegacySnapshotGraph {
    fn resolve_caller_creature(&self, caller: &str) -> LegacyMigrationResult<[u8; 16]> {
        let creature = if self
            .objects
            .contains_key(&("Program".to_owned(), caller.to_owned()))
        {
            self.resolve_program_creature(caller)?
        } else {
            self.object("Creature", caller)?;
            caller.to_owned()
        };
        required_resolved_creature("Bridge", &creature)
    }

    pub(crate) fn transform_legacy_bridges(
        &self,
        migration_time_micros: i64,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let mut capsules = Vec::new();
        let mut claimed = BTreeSet::new();
        for (key, value) in &self.links {
            let Some(topic) = key.strip_prefix("BridgeTopicOwner::") else {
                continue;
            };
            let owner = String::from_utf8(value.clone())
                .ok()
                .filter(|owner| !owner.is_empty() && !topic.is_empty())
                .ok_or_else(|| {
                    LegacyMigrationError::Invalid(format!(
                        "legacy topic claim {topic} is malformed"
                    ))
                })?;
            claimed.insert(topic.to_owned());
            capsules.push(seal_legacy_capsule(
                LegacyCapsuleSpec {
                    family: "BridgeTopic",
                    kind: "core.bridge_topic",
                    storage_class: StorageClass::Core,
                    owner_scope: OwnerScope::Creature(self.resolve_caller_creature(&owner)?),
                    migration_time_micros,
                },
                topic,
                Vec::new(),
                BTreeMap::from([
                    ("topic".to_owned(), CapsuleValue::Text(topic.to_owned())),
                    ("owner_ref".to_owned(), CapsuleValue::Text(owner)),
                ]),
            )?);
        }
        for (key, records) in &self.documents {
            let Some(digest_hex) = key.strip_prefix(LEGACY_BRIDGE_GRANT_PREFIX) else {
                continue;
            };
            let digest = (digest_hex.len() == 64
                && digest_hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
            .then(|| {
                (0..32)
                    .map(|index| u8::from_str_radix(&digest_hex[index * 2..index * 2 + 2], 16))
                    .collect::<Result<Vec<_>, _>>()
                    .ok()
            })
            .flatten()
            .ok_or_else(|| {
                LegacyMigrationError::Invalid(
                    "legacy bridge grant key is not a SHA-256 hex digest".to_owned(),
                )
            })?;
            let grant = verified_legacy_document(key, "grant", records)?;
            let minter = grant
                .get("creatureId")
                .and_then(Value::as_str)
                .filter(|minter| !minter.is_empty())
                .ok_or_else(|| {
                    LegacyMigrationError::Invalid("legacy bridge grant has no minter".to_owned())
                })?
                .to_owned();
            let expires = grant
                .get("expiresAt")
                .map_or(Some(0), Value::as_i64)
                .filter(|expires| *expires >= 0)
                .ok_or_else(|| {
                    LegacyMigrationError::Invalid(
                        "legacy bridge grant expiry is invalid".to_owned(),
                    )
                })?;
            for topic in grant
                .get("topics")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let topic = topic.as_str().unwrap_or("").trim();
                if !topic.is_empty() && !claimed.contains(topic) {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "legacy bridge grant topic {topic} has no BridgeTopicOwner claim"
                    )));
                }
            }
            let entry_count = i64::try_from(grant.len()).map_err(|_| {
                LegacyMigrationError::Invalid("legacy bridge grant is too large".to_owned())
            })?;
            let document = legacy_json_to_capsule_value(key, &Value::Object(grant))?;
            let content_digest = legacy_document_digest(&document)?;
            let expires_micros = expires.checked_mul(1_000).ok_or_else(|| {
                LegacyMigrationError::Invalid("legacy bridge grant expiry overflows".to_owned())
            })?;
            capsules.push(seal_legacy_capsule(
                LegacyCapsuleSpec {
                    family: "BridgeGrant",
                    kind: "core.bridge_grant",
                    storage_class: StorageClass::Core,
                    owner_scope: OwnerScope::Creature(self.resolve_caller_creature(&minter)?),
                    migration_time_micros,
                },
                digest_hex,
                Vec::new(),
                BTreeMap::from([
                    ("token_digest".to_owned(), CapsuleValue::Bytes(digest)),
                    ("minted_by_ref".to_owned(), CapsuleValue::Text(minter)),
                    (
                        "expires_at_micros".to_owned(),
                        CapsuleValue::Integer(expires_micros),
                    ),
                    ("document".to_owned(), document),
                    (
                        "document_path".to_owned(),
                        CapsuleValue::Text("grant".to_owned()),
                    ),
                    ("entry_count".to_owned(), CapsuleValue::Integer(entry_count)),
                    (
                        "content_digest".to_owned(),
                        CapsuleValue::Bytes(content_digest),
                    ),
                ]),
            )?);
        }
        Ok(capsules)
    }
}
