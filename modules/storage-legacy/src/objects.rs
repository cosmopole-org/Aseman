//! Fixture-backed transforms for typed legacy `obj::` families.

use super::*;

/// Fixture-backed transform for the self-contained legacy `Program` object family.
pub fn transform_legacy_program(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    if legacy_id.is_empty() {
        return Err(LegacyMigrationError::Invalid(
            "legacy Program ID is empty".to_owned(),
        ));
    }
    let allowed = ["|", "id", "machineId", "runtime", "path", "comment"];
    if columns
        .keys()
        .any(|column| !allowed.contains(&column.as_str()))
    {
        return Err(LegacyMigrationError::Invalid(
            "legacy Program contains an unreviewed column".to_owned(),
        ));
    }
    if let Some(stored_id) = columns.get("id") {
        let stored_id = utf8_program_column("id", stored_id)?;
        if stored_id != legacy_id {
            return Err(LegacyMigrationError::Invalid(
                "legacy Program key and stored ID disagree".to_owned(),
            ));
        }
    }
    let creature = required_program_column(columns, "machineId")?;
    let runtime = required_program_column(columns, "runtime")?;
    let path = required_program_column(columns, "path")?;
    let comment = columns
        .get("comment")
        .map(|value| utf8_program_column("comment", value))
        .transpose()?
        .unwrap_or_default();
    let creature_id = deterministic_legacy_capsule_id("Creature", creature.as_bytes());
    let capsule = CapsuleEnvelope {
        encoding_version: 1,
        id: CapsuleId(deterministic_legacy_capsule_id(
            "Program",
            legacy_id.as_bytes(),
        )),
        kind: CapsuleKind("core.program".to_owned()),
        storage_class: StorageClass::Core,
        owner_scope: OwnerScope::Creature(creature_id),
        schema_version: 1,
        revision: 1,
        created_at_micros: migration_time_micros,
        updated_at_micros: migration_time_micros,
        previous_integrity: None,
        integrity_hash: CapsuleDigest {
            algorithm: "sha2-256".to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: false,
        relationships: vec![CapsuleRelationship {
            name: "creature".to_owned(),
            target_kind: CapsuleKind("core.creature".to_owned()),
            target_id: CapsuleId(creature_id),
        }],
        body: Some(CapsuleValue::Object(BTreeMap::from([
            ("machine_id".to_owned(), CapsuleValue::Text(creature)),
            ("runtime".to_owned(), CapsuleValue::Text(runtime)),
            ("path".to_owned(), CapsuleValue::Text(path)),
            ("comment".to_owned(), CapsuleValue::Text(comment)),
        ]))),
    };
    capsule
        .seal()
        .map_err(|error| LegacyMigrationError::Contract(error.to_string()))
}

pub(crate) fn required_program_column(
    columns: &BTreeMap<String, Vec<u8>>,
    name: &str,
) -> LegacyMigrationResult<String> {
    let value = columns
        .get(name)
        .ok_or_else(|| LegacyMigrationError::Invalid(format!("legacy Program omits {name}")))?;
    let value = utf8_program_column(name, value)?;
    if value.is_empty() {
        return Err(LegacyMigrationError::Invalid(format!(
            "legacy Program has empty {name}"
        )));
    }
    Ok(value)
}

pub(crate) fn utf8_program_column(name: &str, value: &[u8]) -> LegacyMigrationResult<String> {
    String::from_utf8(value.to_vec())
        .map_err(|_| LegacyMigrationError::Invalid(format!("legacy Program {name} is not UTF-8")))
}

/// Fixture-backed transform for a legacy `Entity` after resolving its program owner.
pub fn transform_legacy_entity(
    legacy_key: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    resolved_creature_legacy_id: &str,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    validate_columns(
        "Entity",
        columns,
        &["|", "programId", "entityId", "entityType", "imageName"],
    )?;
    let program_id = required_utf8_column("Entity", columns, "programId")?;
    let entity_id = required_utf8_column("Entity", columns, "entityId")?;
    let entity_type = required_utf8_column("Entity", columns, "entityType")?;
    let image_name = optional_utf8_column("Entity", columns, "imageName")?;
    if legacy_key != format!("{program_id}::{entity_id}") {
        return Err(LegacyMigrationError::Invalid(
            "legacy Entity key disagrees with its program and entity IDs".to_owned(),
        ));
    }
    let creature_id = required_resolved_creature("Entity", resolved_creature_legacy_id)?;
    let program_capsule_id = deterministic_legacy_capsule_id("Program", program_id.as_bytes());
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "Entity",
            kind: "core.entity",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Creature(creature_id),
            migration_time_micros,
        },
        legacy_key,
        vec![CapsuleRelationship {
            name: "program".to_owned(),
            target_kind: CapsuleKind("core.program".to_owned()),
            target_id: CapsuleId(program_capsule_id),
        }],
        BTreeMap::from([
            ("entity_name".to_owned(), CapsuleValue::Text(entity_id)),
            ("entity_type".to_owned(), CapsuleValue::Text(entity_type)),
            ("image_name".to_owned(), CapsuleValue::Text(image_name)),
        ]),
    )
}

/// Fixture-backed transform for a legacy `Store` after resolving its creator link.
pub fn transform_legacy_store(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    resolved_creator_legacy_id: &str,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    if legacy_id.is_empty() {
        return Err(LegacyMigrationError::Invalid(
            "legacy Store ID is empty".to_owned(),
        ));
    }
    validate_columns(
        "Store",
        columns,
        &[
            "|",
            "tag",
            "parentId",
            "isPublic",
            "persHist",
            "memberCount",
            "signalCount",
        ],
    )?;
    let tag = optional_utf8_column("Store", columns, "tag")?;
    let parent_id = optional_utf8_column("Store", columns, "parentId")?;
    let is_public = required_bool_column("Store", columns, "isPublic")?;
    let persistent_history = required_bool_column("Store", columns, "persHist")?;
    let member_count = i64::from(required_i32_le_column("Store", columns, "memberCount")?);
    let signal_count = required_i64_le_column("Store", columns, "signalCount")?;
    if member_count < 0 || signal_count < 0 {
        return Err(LegacyMigrationError::Invalid(
            "legacy Store contains a negative count".to_owned(),
        ));
    }
    let creature_id = required_resolved_creature("Store", resolved_creator_legacy_id)?;
    let mut relationships = vec![CapsuleRelationship {
        name: "creature".to_owned(),
        target_kind: CapsuleKind("core.creature".to_owned()),
        target_id: CapsuleId(creature_id),
    }];
    if !parent_id.is_empty() {
        if parent_id == legacy_id {
            return Err(LegacyMigrationError::Invalid(
                "legacy Store cannot be its own parent".to_owned(),
            ));
        }
        relationships.push(CapsuleRelationship {
            name: "parent".to_owned(),
            target_kind: CapsuleKind("core.store".to_owned()),
            target_id: CapsuleId(deterministic_legacy_capsule_id(
                "Store",
                parent_id.as_bytes(),
            )),
        });
    }
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "Store",
            kind: "core.store",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Creature(creature_id),
            migration_time_micros,
        },
        legacy_id,
        relationships,
        BTreeMap::from([
            ("is_public".to_owned(), CapsuleValue::Bool(is_public)),
            (
                "member_count".to_owned(),
                CapsuleValue::Integer(member_count),
            ),
            (
                "persistent_history".to_owned(),
                CapsuleValue::Bool(persistent_history),
            ),
            (
                "signal_count".to_owned(),
                CapsuleValue::Integer(signal_count),
            ),
            ("tag".to_owned(), CapsuleValue::Text(tag)),
        ]),
    )
}

/// Fixture-backed transform for a legacy work chain after resolving its store owner.
pub fn transform_legacy_chain(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    resolved_creature_legacy_id: &str,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    validate_columns("Chain", columns, &["|", "id", "storeId"])?;
    let stored_id = required_utf8_column("Chain", columns, "id")?;
    if stored_id != legacy_id {
        return Err(LegacyMigrationError::Invalid(
            "legacy Chain key and stored ID disagree".to_owned(),
        ));
    }
    let store_id = required_utf8_column("Chain", columns, "storeId")?;
    let creature_id = required_resolved_creature("Chain", resolved_creature_legacy_id)?;
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "Chain",
            kind: "core.chain",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Creature(creature_id),
            migration_time_micros,
        },
        legacy_id,
        vec![CapsuleRelationship {
            name: "store".to_owned(),
            target_kind: CapsuleKind("core.store".to_owned()),
            target_id: CapsuleId(deterministic_legacy_capsule_id(
                "Store",
                store_id.as_bytes(),
            )),
        }],
        BTreeMap::from([
            ("store_id".to_owned(), CapsuleValue::Text(store_id)),
            ("status".to_owned(), CapsuleValue::Text("active".to_owned())),
        ]),
    )
}

/// Fixture-backed transform for a named legacy shard after resolving its chain owner.
pub fn transform_legacy_chain_shard(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    resolved_creature_legacy_id: &str,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    validate_columns("ChainShard", columns, &["|", "id", "workChainId"])?;
    let stored_id = required_utf8_column("ChainShard", columns, "id")?;
    if stored_id != legacy_id {
        return Err(LegacyMigrationError::Invalid(
            "legacy ChainShard key and stored ID disagree".to_owned(),
        ));
    }
    let chain_id = required_utf8_column("ChainShard", columns, "workChainId")?;
    let creature_id = required_resolved_creature("ChainShard", resolved_creature_legacy_id)?;
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "ChainShard",
            kind: "core.chain_shard",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Creature(creature_id),
            migration_time_micros,
        },
        legacy_id,
        vec![CapsuleRelationship {
            name: "chain".to_owned(),
            target_kind: CapsuleKind("core.chain".to_owned()),
            target_id: CapsuleId(deterministic_legacy_capsule_id(
                "Chain",
                chain_id.as_bytes(),
            )),
        }],
        BTreeMap::from([
            ("work_chain_id".to_owned(), CapsuleValue::Text(chain_id)),
            (
                "shard_name".to_owned(),
                CapsuleValue::Text(legacy_id.to_owned()),
            ),
        ]),
    )
}

/// Convert a legacy bearer session into a target revocation marker.
///
/// Legacy sessions have no trustworthy issue or expiry timestamps. They are never made
/// live in the target: the digest is retained with zero unknown timestamps and an
/// explicit migration-time revocation so rollback/cutover checks can deny replay.
pub fn transform_legacy_session_revocation(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    resolved_user_legacy_id: &str,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    validate_columns("Session", columns, &["|", "userId"])?;
    let _subject_creature_id = required_utf8_column("Session", columns, "userId")?;
    if legacy_id.is_empty() || resolved_user_legacy_id.is_empty() {
        return Err(LegacyMigrationError::Invalid(
            "legacy Session ID or resolved target user is empty".to_owned(),
        ));
    }
    let mut digest = Sha256::new();
    digest.update(b"ASEMAN-LEGACY-SESSION-REVOCATION-V1\0");
    digest.update((legacy_id.len() as u64).to_be_bytes());
    digest.update(legacy_id.as_bytes());
    let token_digest = digest.finalize().to_vec();
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "Session",
            kind: "core.session",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Global,
            migration_time_micros,
        },
        legacy_id,
        vec![CapsuleRelationship {
            name: "user".to_owned(),
            target_kind: CapsuleKind("core.user".to_owned()),
            target_id: CapsuleId(deterministic_legacy_capsule_id(
                "User",
                resolved_user_legacy_id.as_bytes(),
            )),
        }],
        BTreeMap::from([
            ("token_digest".to_owned(), CapsuleValue::Bytes(token_digest)),
            ("issued_at_micros".to_owned(), CapsuleValue::Integer(0)),
            ("expires_at_micros".to_owned(), CapsuleValue::Integer(0)),
            (
                "revoked_at_micros".to_owned(),
                CapsuleValue::Integer(migration_time_micros),
            ),
        ]),
    )
}

/// Verified metadata for a legacy filesystem object copied by the migration runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyFileArtifactEvidence {
    pub store_key: String,
    pub content_digest: [u8; 32],
    pub media_type: String,
    pub size_bytes: u64,
}

impl LegacyFileArtifactEvidence {
    pub fn from_bytes(
        store_key: &str,
        media_type: &str,
        content: &[u8],
    ) -> LegacyMigrationResult<Self> {
        if store_key.is_empty() || store_key.contains('\0') {
            return Err(LegacyMigrationError::Invalid(
                "legacy file store key is empty or contains NUL".to_owned(),
            ));
        }
        if content.len() > DEFAULT_MAX_LEGACY_FILE_BYTES {
            return Err(LegacyMigrationError::Invalid(
                "legacy file exceeds the migration byte bound".to_owned(),
            ));
        }
        let media_type = if media_type.is_empty() {
            "application/octet-stream"
        } else {
            media_type
        };
        Ok(Self {
            store_key: store_key.to_owned(),
            content_digest: Sha256::digest(content).into(),
            media_type: media_type.to_owned(),
            size_bytes: u64::try_from(content.len()).map_err(|_| {
                LegacyMigrationError::Invalid("legacy file size overflows u64".to_owned())
            })?,
        })
    }
}

/// Transform legacy file metadata only after its external bytes have digest evidence.
pub fn transform_legacy_file(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    artifact: &LegacyFileArtifactEvidence,
    resolved_owner_user_legacy_id: &str,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    validate_columns("File", columns, &["|", "storeId", "ownerId"])?;
    let owner_creature_id = required_utf8_column("File", columns, "ownerId")?;
    let store_id = optional_utf8_column("File", columns, "storeId")?;
    if artifact.store_key.is_empty()
        || artifact.content_digest == [0; 32]
        || resolved_owner_user_legacy_id.is_empty()
    {
        return Err(LegacyMigrationError::Invalid(
            "legacy File artifact evidence is incomplete".to_owned(),
        ));
    }
    let size_bytes = i64::try_from(artifact.size_bytes).map_err(|_| {
        LegacyMigrationError::Invalid("legacy File size exceeds target integer range".to_owned())
    })?;
    let creature_id = deterministic_legacy_capsule_id("Creature", owner_creature_id.as_bytes());
    let mut relationships = vec![CapsuleRelationship {
        name: "owner".to_owned(),
        target_kind: CapsuleKind("core.user".to_owned()),
        target_id: CapsuleId(deterministic_legacy_capsule_id(
            "User",
            resolved_owner_user_legacy_id.as_bytes(),
        )),
    }];
    if !store_id.is_empty() {
        relationships.push(CapsuleRelationship {
            name: "store".to_owned(),
            target_kind: CapsuleKind("core.store".to_owned()),
            target_id: CapsuleId(deterministic_legacy_capsule_id(
                "Store",
                store_id.as_bytes(),
            )),
        });
    }
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "File",
            kind: "core.file",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Creature(creature_id),
            migration_time_micros,
        },
        legacy_id,
        relationships,
        BTreeMap::from([
            (
                "store_key".to_owned(),
                CapsuleValue::Text(artifact.store_key.clone()),
            ),
            (
                "content_digest".to_owned(),
                CapsuleValue::Bytes(artifact.content_digest.to_vec()),
            ),
            (
                "media_type".to_owned(),
                CapsuleValue::Text(artifact.media_type.clone()),
            ),
            ("size_bytes".to_owned(), CapsuleValue::Integer(size_bytes)),
        ]),
    )
}

/// Convert a legacy RSA SubjectPublicKeyInfo PEM into tagged multicodec bytes.
pub fn encode_legacy_rsa_public_key(public_key_pem: &str) -> LegacyMigrationResult<Vec<u8>> {
    let key = RsaPublicKey::from_public_key_pem(public_key_pem).map_err(|_| {
        LegacyMigrationError::Invalid("legacy Creature publicKey is not RSA SPKI PEM".to_owned())
    })?;
    let der = key.to_public_key_der().map_err(|_| {
        LegacyMigrationError::Invalid("legacy Creature RSA key cannot encode as SPKI".to_owned())
    })?;
    // Multicodec rsa-pub (0x1205), unsigned-varint encoded as 0x85 0x24.
    let mut encoded = Vec::with_capacity(2 + der.as_bytes().len());
    encoded.extend_from_slice(&[0x85, 0x24]);
    encoded.extend_from_slice(der.as_bytes());
    Ok(encoded)
}

/// Split one legacy unified Creature into target identity, boundary, and wallet records.
pub fn transform_legacy_creature(
    legacy_id: &str,
    columns: &BTreeMap<String, Vec<u8>>,
    resolved_owner_user_legacy_id: &str,
    email: Option<&str>,
    finance: &LegacyFinanceConfig,
    migration_time_micros: i64,
) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
    validate_columns(
        "Creature",
        columns,
        &[
            "|",
            "type",
            "username",
            "publicKey",
            "chainId",
            "subchainId",
            "ownerId",
            "balance",
        ],
    )?;
    if legacy_id.is_empty() || resolved_owner_user_legacy_id.is_empty() {
        return Err(LegacyMigrationError::Invalid(
            "legacy Creature identity or resolved owner is empty".to_owned(),
        ));
    }
    let creature_type = required_utf8_column("Creature", columns, "type")?;
    let username = required_utf8_column("Creature", columns, "username")?;
    let public_key_pem = required_utf8_column("Creature", columns, "publicKey")?;
    let public_key = encode_legacy_rsa_public_key(&public_key_pem)?;
    let chain_id = optional_utf8_column("Creature", columns, "chainId")?;
    let subchain_id = optional_utf8_column("Creature", columns, "subchainId")?;
    let balance = required_i64_le_column("Creature", columns, "balance")?;
    if finance.currency.is_empty()
        || finance.currency.len() > 16
        || !finance
            .currency
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        || finance.scale > 18
    {
        return Err(LegacyMigrationError::Invalid(
            "legacy finance currency/scale configuration is invalid".to_owned(),
        ));
    }
    let creature_id = deterministic_legacy_capsule_id("Creature", legacy_id.as_bytes());
    let owner_user_id =
        deterministic_legacy_capsule_id("User", resolved_owner_user_legacy_id.as_bytes());
    let mut capsules = Vec::new();
    if creature_type == "human" {
        if resolved_owner_user_legacy_id != legacy_id {
            return Err(LegacyMigrationError::Invalid(
                "legacy human Creature must resolve to its own target user".to_owned(),
            ));
        }
        capsules.push(seal_legacy_capsule(
            LegacyCapsuleSpec {
                family: "User",
                kind: "core.user",
                storage_class: StorageClass::Core,
                owner_scope: OwnerScope::Global,
                migration_time_micros,
            },
            legacy_id,
            Vec::new(),
            BTreeMap::from([
                ("username".to_owned(), CapsuleValue::Text(username.clone())),
                (
                    "email".to_owned(),
                    CapsuleValue::Text(email.unwrap_or_default().to_owned()),
                ),
                (
                    "public_key".to_owned(),
                    CapsuleValue::Bytes(public_key.clone()),
                ),
                ("status".to_owned(), CapsuleValue::Text("active".to_owned())),
            ]),
        )?);
    }
    capsules.push(seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "Creature",
            kind: "core.creature",
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Global,
            migration_time_micros,
        },
        legacy_id,
        vec![CapsuleRelationship {
            name: "owner".to_owned(),
            target_kind: CapsuleKind("core.user".to_owned()),
            target_id: CapsuleId(owner_user_id),
        }],
        BTreeMap::from([
            ("username".to_owned(), CapsuleValue::Text(username)),
            (
                "creature_type".to_owned(),
                CapsuleValue::Text(creature_type),
            ),
            ("public_key".to_owned(), CapsuleValue::Bytes(public_key)),
            ("status".to_owned(), CapsuleValue::Text("active".to_owned())),
            ("chain_id".to_owned(), CapsuleValue::Text(chain_id)),
            ("subchain_id".to_owned(), CapsuleValue::Text(subchain_id)),
        ]),
    )?);
    let mut wallet_source_id = Vec::with_capacity(legacy_id.len() + finance.currency.len() + 1);
    wallet_source_id.extend_from_slice(legacy_id.as_bytes());
    wallet_source_id.push(0);
    wallet_source_id.extend_from_slice(finance.currency.as_bytes());
    let wallet_id = deterministic_legacy_capsule_id("Wallet", &wallet_source_id);
    capsules.push(
        CapsuleEnvelope {
            encoding_version: 1,
            id: CapsuleId(wallet_id),
            kind: CapsuleKind("finance.wallet".to_owned()),
            storage_class: StorageClass::Finance,
            owner_scope: OwnerScope::Creature(creature_id),
            schema_version: 1,
            revision: 1,
            created_at_micros: migration_time_micros,
            updated_at_micros: migration_time_micros,
            previous_integrity: None,
            integrity_hash: CapsuleDigest {
                algorithm: "sha2-256".to_owned(),
                bytes: vec![0; 32],
            },
            tombstone: false,
            relationships: vec![CapsuleRelationship {
                name: "creature".to_owned(),
                target_kind: CapsuleKind("core.creature".to_owned()),
                target_id: CapsuleId(creature_id),
            }],
            body: Some(CapsuleValue::Object(BTreeMap::from([
                (
                    "currency".to_owned(),
                    CapsuleValue::Text(finance.currency.clone()),
                ),
                ("balance_minor".to_owned(), CapsuleValue::Integer(balance)),
                (
                    "scale".to_owned(),
                    CapsuleValue::Integer(i64::from(finance.scale)),
                ),
                ("state".to_owned(), CapsuleValue::Text("active".to_owned())),
            ]))),
        }
        .seal()
        .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?,
    );
    Ok(capsules)
}
