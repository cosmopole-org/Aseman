//! Fixture-backed tests for the legacy capsule migration bridge.

use super::*;
use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleId, CapsuleKind, CapsuleValue, OwnerScope, StorageClass,
};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

/// RSA-2048 generation is slow in debug builds, so each test binary generates a small
/// pool of distinct `(pkcs8 private PEM, SPKI public PEM)` pairs once.
fn test_rsa_key_pair(slot: usize) -> (String, String) {
    use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
    use rsa::rand_core::OsRng;
    static PAIRS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    PAIRS.get_or_init(|| {
        (0..3)
            .map(|_| {
                let private = rsa::RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
                (
                    private.to_pkcs8_pem(LineEnding::LF).unwrap().to_string(),
                    rsa::RsaPublicKey::from(&private)
                        .to_public_key_pem(LineEnding::LF)
                        .unwrap(),
                )
            })
            .collect()
    })[slot]
        .clone()
}

fn test_rsa_public_key_pem() -> String {
    test_rsa_key_pair(0).1
}

struct MemorySource(Vec<LegacyPhysicalRecord>);

impl LegacyRecordSource for MemorySource {
    fn read_snapshot(
        &self,
        max_records: usize,
        max_bytes: usize,
    ) -> LegacyMigrationResult<LegacySnapshot> {
        let bytes = self
            .0
            .iter()
            .map(|record| record.key.len() + record.value.len())
            .sum::<usize>();
        if self.0.len() > max_records || bytes > max_bytes {
            return Err(LegacyMigrationError::Invalid(
                "fixture exceeds bounds".to_owned(),
            ));
        }
        Ok(LegacySnapshot {
            snapshot_id: "fixture".to_owned(),
            records: self.0.clone(),
        })
    }
}

struct FixtureTransformer;

impl LegacyTransformer for FixtureTransformer {
    fn manifest_digest(&self) -> [u8; 32] {
        [7; 32]
    }

    fn transform(
        &mut self,
        record: LegacyPhysicalRecord,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        if record.key != b"known" {
            return Err(LegacyMigrationError::Unmapped {
                family: record.family,
                key: String::from_utf8_lossy(&record.key).into_owned(),
            });
        }
        let mut capsule = CapsuleEnvelope {
            encoding_version: 1,
            id: CapsuleId(deterministic_legacy_capsule_id("fixture", &record.value)),
            kind: CapsuleKind("core.user".to_owned()),
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Global,
            schema_version: 1,
            revision: 1,
            created_at_micros: 1,
            updated_at_micros: 1,
            previous_integrity: None,
            integrity_hash: CapsuleDigest {
                algorithm: "sha2-256".to_owned(),
                bytes: vec![0; 32],
            },
            tombstone: false,
            relationships: Vec::new(),
            body: Some(CapsuleValue::Object(BTreeMap::from([(
                "username".to_owned(),
                CapsuleValue::Text(String::from_utf8_lossy(&record.value).into_owned()),
            )]))),
        };
        capsule = capsule
            .seal()
            .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?;
        Ok(vec![capsule])
    }

    fn finish(&mut self) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
struct MemorySink(BTreeSet<Vec<u8>>);

impl CapsuleImportSink for MemorySink {
    fn import(&mut self, capsule: CapsuleEnvelope) -> LegacyMigrationResult<ImportDisposition> {
        let bytes = capsule
            .canonical_bytes()
            .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?;
        Ok(if self.0.insert(bytes) {
            ImportDisposition::Inserted
        } else {
            ImportDisposition::AlreadyPresent
        })
    }
}

#[test]
fn bounded_export_import_is_deterministic_resumable_and_idempotent() {
    let source = MemorySource(vec![LegacyPhysicalRecord {
        family: "fixture".to_owned(),
        key: b"known".to_vec(),
        value: b"alice".to_vec(),
    }]);
    let export = export_canonical(
        "legacy-rocksdb-v1",
        10,
        &source,
        &mut FixtureTransformer,
        10,
        1_024,
    )
    .unwrap();
    let mut sink = MemorySink::default();
    let first = import_canonical(&export, None, &mut sink, 1).unwrap();
    assert_eq!(first.inserted, 1);
    assert_eq!(first.checkpoint.next_sequence, 1);
    let complete = import_canonical(&export, Some(&first.checkpoint), &mut sink, 1).unwrap();
    assert_eq!(complete.inserted, 0);
    let replay = import_canonical(&export, None, &mut sink, 1).unwrap();
    assert_eq!(replay.already_present, 1);
}

#[test]
fn unmapped_records_and_forged_checkpoints_fail_closed() {
    let source = MemorySource(vec![LegacyPhysicalRecord {
        family: "fixture".to_owned(),
        key: b"unknown".to_vec(),
        value: Vec::new(),
    }]);
    assert!(matches!(
        export_canonical(
            "legacy-rocksdb-v1",
            10,
            &source,
            &mut FixtureTransformer,
            10,
            1_024
        ),
        Err(LegacyMigrationError::Unmapped { .. })
    ));

    let good = MemorySource(vec![LegacyPhysicalRecord {
        family: "fixture".to_owned(),
        key: b"known".to_vec(),
        value: b"alice".to_vec(),
    }]);
    let export = export_canonical(
        "legacy-rocksdb-v1",
        10,
        &good,
        &mut FixtureTransformer,
        10,
        1_024,
    )
    .unwrap();
    let mut checkpoint = export.checkpoint(0).unwrap();
    checkpoint.stream_digest[0] ^= 1;
    assert!(import_canonical(&export, Some(&checkpoint), &mut MemorySink::default(), 1).is_err());
}

#[test]
fn legacy_program_fixture_maps_identity_owner_relationship_and_fields() {
    let columns = BTreeMap::from([
        ("|".to_owned(), vec![1]),
        ("id".to_owned(), b"program-one".to_vec()),
        ("machineId".to_owned(), b"creature-one".to_vec()),
        ("runtime".to_owned(), b"wasm".to_vec()),
        ("path".to_owned(), b"/programs/one".to_vec()),
        ("comment".to_owned(), b"fixture".to_vec()),
    ]);
    let capsule = transform_legacy_program("program-one", &columns, 50).unwrap();
    capsule.verify().unwrap();
    assert_eq!(capsule.kind.0, "core.program");
    let creature_id = deterministic_legacy_capsule_id("Creature", b"creature-one");
    assert_eq!(capsule.owner_scope, OwnerScope::Creature(creature_id));
    assert_eq!(capsule.relationships[0].target_id.0, creature_id);
    assert!(matches!(
        &capsule.body,
        Some(CapsuleValue::Object(body))
            if body["runtime"] == CapsuleValue::Text("wasm".to_owned())
    ));

    let mut unknown = columns;
    unknown.insert("unreviewed".to_owned(), Vec::new());
    assert!(transform_legacy_program("program-one", &unknown, 50).is_err());
}

#[test]
fn legacy_build_log_requires_resolved_owner_and_converts_millis() {
    let row = LegacyBuildLogRow {
        id: "log-one".to_owned(),
        build_id: String::new(),
        machine_id: String::new(),
        vm_id: "vm-one".to_owned(),
        log_type: "runtime".to_owned(),
        data: "ready".to_owned(),
        time_millis: 1_700_000_000_123,
    };
    let capsule = transform_legacy_build_log(&row, "creature-one").unwrap();
    capsule.verify().unwrap();
    assert_eq!(capsule.kind.0, "telemetry.build_log");
    assert_eq!(capsule.storage_class, StorageClass::Telemetry);
    assert_eq!(capsule.created_at_micros, 1_700_000_000_123_000);
    assert_eq!(
        capsule.owner_scope,
        OwnerScope::Creature(deterministic_legacy_capsule_id("Creature", b"creature-one"))
    );
    assert!(matches!(
        &capsule.body,
        Some(CapsuleValue::Object(body))
            if body["workload_id"] == CapsuleValue::Text("vm-one".to_owned())
                && body["machine_id"] == CapsuleValue::Text("creature-one".to_owned())
                && body["observed_at_micros"]
                    == CapsuleValue::Integer(1_700_000_000_123_000)
    ));

    let mut conflicting = row.clone();
    conflicting.machine_id = "other-creature".to_owned();
    assert!(transform_legacy_build_log(&conflicting, "creature-one").is_err());
    assert!(transform_legacy_build_log(&row, "").is_err());
}

#[test]
fn legacy_signal_history_gets_deterministic_per_store_sequences() {
    let policies = BTreeMap::from([
        (
            "store-a".to_owned(),
            LegacySignalStreamPolicy {
                authorization_scope: b"scope-a".to_vec(),
                retention_class: "permanent".to_owned(),
            },
        ),
        (
            "store-b".to_owned(),
            LegacySignalStreamPolicy {
                authorization_scope: b"scope-b".to_vec(),
                retention_class: "bounded".to_owned(),
            },
        ),
    ]);
    let rows = vec![
        LegacySignalRow {
            id: "later".to_owned(),
            store_id: "store-a".to_owned(),
            user_id: "user-a".to_owned(),
            data: "two".to_owned(),
            encoded_tags: "|kind=message|thread=main|".to_owned(),
            time_millis: 20,
            edited: true,
        },
        LegacySignalRow {
            id: "other".to_owned(),
            store_id: "store-b".to_owned(),
            user_id: "user-b".to_owned(),
            data: "other".to_owned(),
            encoded_tags: String::new(),
            time_millis: 30,
            edited: false,
        },
        LegacySignalRow {
            id: "earlier".to_owned(),
            store_id: "store-a".to_owned(),
            user_id: "user-a".to_owned(),
            data: "one".to_owned(),
            encoded_tags: "|kind=message|".to_owned(),
            time_millis: 10,
            edited: false,
        },
    ];
    let capsules = transform_legacy_signal_rows(rows, &policies).unwrap();
    assert_eq!(capsules.len(), 3);
    assert!(capsules.iter().all(|capsule| capsule.verify().is_ok()));
    let sequences = capsules
        .iter()
        .map(|capsule| match &capsule.body {
            Some(CapsuleValue::Object(body)) => (
                body["stream_id"].clone(),
                body["sequence"].clone(),
                body["occurred_at_micros"].clone(),
            ),
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sequences,
        vec![
            (
                CapsuleValue::Text("store:store-a".to_owned()),
                CapsuleValue::Integer(1),
                CapsuleValue::Integer(10_000),
            ),
            (
                CapsuleValue::Text("store:store-a".to_owned()),
                CapsuleValue::Integer(2),
                CapsuleValue::Integer(20_000),
            ),
            (
                CapsuleValue::Text("store:store-b".to_owned()),
                CapsuleValue::Integer(1),
                CapsuleValue::Integer(30_000),
            ),
        ]
    );
    assert!(
        transform_legacy_signal_rows(
            vec![LegacySignalRow {
                id: "bad".to_owned(),
                store_id: "store-a".to_owned(),
                user_id: "user-a".to_owned(),
                data: String::new(),
                encoded_tags: "not-framed".to_owned(),
                time_millis: 1,
                edited: false,
            }],
            &policies,
        )
        .is_err()
    );
}

#[test]
fn legacy_entity_maps_composite_identity_program_and_resolved_owner() {
    let columns = BTreeMap::from([
        ("|".to_owned(), vec![1]),
        ("programId".to_owned(), b"program-one".to_vec()),
        ("entityId".to_owned(), b"worker".to_vec()),
        ("entityType".to_owned(), b"container".to_vec()),
        ("imageName".to_owned(), b"worker:v1".to_vec()),
    ]);
    let capsule =
        transform_legacy_entity("program-one::worker", &columns, "creature-one", 60).unwrap();
    capsule.verify().unwrap();
    assert_eq!(capsule.kind.0, "core.entity");
    assert_eq!(
        capsule.relationships[0].target_id.0,
        deterministic_legacy_capsule_id("Program", b"program-one")
    );
    assert!(transform_legacy_entity("wrong", &columns, "creature-one", 60).is_err());
    assert!(transform_legacy_entity("program-one::worker", &columns, "", 60).is_err());
}

#[test]
fn legacy_store_decodes_binary_fields_and_resolved_creator_link() {
    let columns = BTreeMap::from([
        ("|".to_owned(), vec![1]),
        ("tag".to_owned(), b"orders".to_vec()),
        ("parentId".to_owned(), b"root-store".to_vec()),
        ("isPublic".to_owned(), vec![1]),
        ("persHist".to_owned(), vec![0]),
        ("memberCount".to_owned(), 3_i32.to_le_bytes().to_vec()),
        ("signalCount".to_owned(), 9_i64.to_le_bytes().to_vec()),
    ]);
    let capsule = transform_legacy_store("child-store", &columns, "creature-one", 70).unwrap();
    capsule.verify().unwrap();
    assert_eq!(capsule.kind.0, "core.store");
    assert_eq!(capsule.relationships.len(), 2);
    assert!(matches!(
        &capsule.body,
        Some(CapsuleValue::Object(body))
            if body["is_public"] == CapsuleValue::Bool(true)
                && body["member_count"] == CapsuleValue::Integer(3)
                && body["signal_count"] == CapsuleValue::Integer(9)
    ));

    let mut malformed = columns.clone();
    malformed.insert("isPublic".to_owned(), vec![2]);
    assert!(transform_legacy_store("child-store", &malformed, "creature-one", 70).is_err());
    assert!(transform_legacy_store("child-store", &columns, "", 70).is_err());
}

#[test]
fn legacy_chain_and_named_shard_preserve_graph_and_owner() {
    let chain_columns = BTreeMap::from([
        ("|".to_owned(), vec![1]),
        ("id".to_owned(), b"work-chain".to_vec()),
        ("storeId".to_owned(), b"store-one".to_vec()),
    ]);
    let chain = transform_legacy_chain("work-chain", &chain_columns, "creature-one", 80).unwrap();
    chain.verify().unwrap();
    assert_eq!(chain.kind.0, "core.chain");
    assert!(matches!(
        &chain.body,
        Some(CapsuleValue::Object(body))
            if body["status"] == CapsuleValue::Text("active".to_owned())
    ));

    let shard_columns = BTreeMap::from([
        ("|".to_owned(), vec![1]),
        ("id".to_owned(), b"shard-main".to_vec()),
        ("workChainId".to_owned(), b"work-chain".to_vec()),
    ]);
    let shard =
        transform_legacy_chain_shard("shard-main", &shard_columns, "creature-one", 80).unwrap();
    shard.verify().unwrap();
    assert_eq!(shard.kind.0, "core.chain_shard");
    assert_eq!(shard.relationships[0].target_id.0, chain.id.0);
    assert!(matches!(
        &shard.body,
        Some(CapsuleValue::Object(body))
            if body["shard_name"] == CapsuleValue::Text("shard-main".to_owned())
    ));
}

#[test]
fn snapshot_graph_resolves_owners_independent_of_physical_order() {
    let mut records = Vec::new();
    let mut add_object = |family: &str, id: &str, columns: BTreeMap<&str, Vec<u8>>| {
        for (column, value) in columns {
            records.push(LegacyPhysicalRecord {
                family: "application-rocksdb-default".to_owned(),
                key: format!("obj::{family}::{id}::{column}").into_bytes(),
                value,
            });
        }
    };
    add_object(
        "Program",
        "program-one",
        BTreeMap::from([
            ("|", vec![1]),
            ("id", b"program-one".to_vec()),
            ("machineId", b"creature-one".to_vec()),
            ("runtime", b"wasm".to_vec()),
            ("path", b"/one".to_vec()),
            ("comment", Vec::new()),
        ]),
    );
    add_object(
        "Entity",
        "program-one::worker",
        BTreeMap::from([
            ("|", vec![1]),
            ("programId", b"program-one".to_vec()),
            ("entityId", b"worker".to_vec()),
            ("entityType", b"wasm".to_vec()),
            ("imageName", b"worker".to_vec()),
        ]),
    );
    add_object(
        "Store",
        "store-one",
        BTreeMap::from([
            ("|", vec![1]),
            ("tag", b"events".to_vec()),
            ("parentId", Vec::new()),
            ("isPublic", vec![0]),
            ("persHist", vec![1]),
            ("memberCount", 1_i32.to_le_bytes().to_vec()),
            ("signalCount", 2_i64.to_le_bytes().to_vec()),
        ]),
    );
    add_object(
        "Chain",
        "chain-one",
        BTreeMap::from([
            ("|", vec![1]),
            ("id", b"chain-one".to_vec()),
            ("storeId", b"store-one".to_vec()),
        ]),
    );
    add_object(
        "ChainShard",
        "shard-main",
        BTreeMap::from([
            ("|", vec![1]),
            ("id", b"shard-main".to_vec()),
            ("workChainId", b"chain-one".to_vec()),
        ]),
    );
    records.push(LegacyPhysicalRecord {
        family: "application-rocksdb-default".to_owned(),
        key: b"link::creatorof::creature-one::store-one".to_vec(),
        value: b"true".to_vec(),
    });
    records.push(LegacyPhysicalRecord {
        family: "application-rocksdb-default".to_owned(),
        key: b"link::machinePrograms::creature-one::program-one".to_vec(),
        value: b"true".to_vec(),
    });
    records.reverse();

    let graph = LegacySnapshotGraph::assemble(records).unwrap();
    let capsules = graph.transform_reviewed(90).unwrap();
    // Five entities, each with its legacy identity record (ADR 0009 legacy-ID map).
    let identities = capsules
        .iter()
        .filter(|capsule| capsule.kind.0 == "core.legacy_identity")
        .count();
    assert_eq!((capsules.len() - identities, identities), (5, 5));
    assert!(capsules.windows(2).all(|pair| {
        (pair[0].kind.0.as_str(), pair[0].id.0) <= (pair[1].kind.0.as_str(), pair[1].id.0)
    }));
    assert!(capsules.iter().all(|capsule| capsule.verify().is_ok()));
}

#[test]
fn snapshot_graph_rejects_raw_keys_and_unreviewed_typed_families() {
    assert!(matches!(
        LegacySnapshotGraph::assemble(vec![LegacyPhysicalRecord {
            family: "application-rocksdb-default".to_owned(),
            key: b"unreviewedRawKey".to_vec(),
            value: vec![1],
        }]),
        Err(LegacyMigrationError::Unmapped { .. })
    ));
    let graph = LegacySnapshotGraph::assemble(vec![LegacyPhysicalRecord {
        family: "application-rocksdb-default".to_owned(),
        key: b"obj::File::file-one::ownerId".to_vec(),
        value: b"user-one".to_vec(),
    }])
    .unwrap();
    assert!(matches!(
        graph.transform_reviewed(100),
        Err(LegacyMigrationError::Unmapped { family, .. }) if family == "File.artifact"
    ));
}

#[test]
fn legacy_session_becomes_deterministic_revocation_not_live_credential() {
    let columns = BTreeMap::from([
        ("|".to_owned(), vec![1]),
        ("userId".to_owned(), b"user-one".to_vec()),
    ]);
    let capsule =
        transform_legacy_session_revocation("secret-session", &columns, "user-one", 110).unwrap();
    capsule.verify().unwrap();
    assert_eq!(capsule.kind.0, "core.session");
    assert!(matches!(
        &capsule.body,
        Some(CapsuleValue::Object(body))
            if body["issued_at_micros"] == CapsuleValue::Integer(0)
                && body["expires_at_micros"] == CapsuleValue::Integer(0)
                && body["revoked_at_micros"] == CapsuleValue::Integer(110)
                && matches!(&body["token_digest"], CapsuleValue::Bytes(bytes) if bytes.len() == 32)
    ));
    assert_ne!(
        match capsule.body.unwrap() {
            CapsuleValue::Object(body) => body["token_digest"].clone(),
            _ => unreachable!(),
        },
        CapsuleValue::Bytes(b"secret-session".to_vec())
    );
}

#[test]
fn legacy_file_requires_digest_evidence_for_external_bytes() {
    let columns = BTreeMap::from([
        ("|".to_owned(), vec![1]),
        ("storeId".to_owned(), b"store-one".to_vec()),
        ("ownerId".to_owned(), b"user-one".to_vec()),
    ]);
    let evidence =
        LegacyFileArtifactEvidence::from_bytes("files/store-one/file-one", "text/plain", b"hello")
            .unwrap();
    let capsule = transform_legacy_file("file-one", &columns, &evidence, "user-one", 120).unwrap();
    capsule.verify().unwrap();
    assert_eq!(capsule.kind.0, "core.file");
    assert_eq!(capsule.relationships.len(), 2);
    assert!(matches!(
        &capsule.body,
        Some(CapsuleValue::Object(body))
            if body["size_bytes"] == CapsuleValue::Integer(5)
                && matches!(&body["content_digest"], CapsuleValue::Bytes(bytes) if bytes.len() == 32)
    ));
}

#[test]
fn legacy_creature_splits_human_boundary_and_configured_wallet() {
    let public_key = test_rsa_public_key_pem();
    let columns = BTreeMap::from([
        ("|".to_owned(), vec![1]),
        ("type".to_owned(), b"human".to_vec()),
        ("username".to_owned(), b"alice@example".to_vec()),
        ("publicKey".to_owned(), public_key.as_bytes().to_vec()),
        ("chainId".to_owned(), b"main".to_vec()),
        ("subchainId".to_owned(), b"shard-main".to_vec()),
        ("ownerId".to_owned(), b"free".to_vec()),
        ("balance".to_owned(), 125_i64.to_le_bytes().to_vec()),
    ]);
    let capsules = transform_legacy_creature(
        "human-one",
        &columns,
        "human-one",
        Some("alice@example.test"),
        &LegacyFinanceConfig {
            currency: "ASE".to_owned(),
            scale: 2,
        },
        130,
    )
    .unwrap();
    assert_eq!(capsules.len(), 3);
    assert_eq!(capsules[0].kind.0, "core.user");
    assert_eq!(capsules[1].kind.0, "core.creature");
    // A human who never logged in by email has no email at all: `email` is unique,
    // so an empty string would collide between such users at import.
    let without_email = transform_legacy_creature(
        "human-one",
        &columns,
        "human-one",
        None,
        &LegacyFinanceConfig {
            currency: "ASE".to_owned(),
            scale: 2,
        },
        130,
    )
    .unwrap();
    assert!(matches!(
        &without_email[0].body,
        Some(CapsuleValue::Object(body)) if !body.contains_key("email")
    ));
    assert_eq!(capsules[2].kind.0, "finance.wallet");
    assert!(capsules.iter().all(|capsule| capsule.verify().is_ok()));
    assert!(matches!(
        &capsules[1].body,
        Some(CapsuleValue::Object(body))
            if body["chain_id"] == CapsuleValue::Text("main".to_owned())
                && matches!(&body["public_key"], CapsuleValue::Bytes(bytes) if bytes.starts_with(&[0x85, 0x24]))
    ));
    assert!(matches!(
        &capsules[2].body,
        Some(CapsuleValue::Object(body))
            if body["balance_minor"] == CapsuleValue::Integer(125)
                && body["currency"] == CapsuleValue::Text("ASE".to_owned())
                && body["scale"] == CapsuleValue::Integer(2)
    ));
    assert!(
        transform_legacy_creature(
            "human-one",
            &columns,
            "other-user",
            None,
            &LegacyFinanceConfig {
                currency: "ASE".to_owned(),
                scale: 2,
            },
            130,
        )
        .is_err()
    );
}

/// Mirrors legacy `TrxWrapper::index_json` (node/src/core/actor/model/trx.rs)
/// for a first write: the document plus every non-null member splat.
fn legacy_put_json(
    records: &mut Vec<LegacyPhysicalRecord>,
    key: &str,
    path: &str,
    document: &serde_json::Value,
) {
    records.push(LegacyPhysicalRecord {
        family: "application-rocksdb-default".to_owned(),
        key: format!("json::{key}::{path}").into_bytes(),
        value: serde_json::to_vec(document).unwrap(),
    });
    if let serde_json::Value::Object(members) = document {
        for (member, value) in members {
            match value {
                serde_json::Value::Null => {}
                serde_json::Value::Object(_) => {
                    legacy_put_json(records, key, &format!("{path}.{member}"), value);
                }
                other => records.push(LegacyPhysicalRecord {
                    family: "application-rocksdb-default".to_owned(),
                    key: format!("json::{key}::{path}.{member}").into_bytes(),
                    value: serde_json::to_vec(other).unwrap(),
                }),
            }
        }
    }
}

fn metadata_fixture_subjects() -> Vec<LegacyPhysicalRecord> {
    let public_key = test_rsa_public_key_pem();
    let mut records = Vec::new();
    let mut add_object = |family: &str, id: &str, columns: Vec<(&str, Vec<u8>)>| {
        for (column, value) in columns {
            records.push(LegacyPhysicalRecord {
                family: "application-rocksdb-default".to_owned(),
                key: format!("obj::{family}::{id}::{column}").into_bytes(),
                value,
            });
        }
    };
    add_object(
        "Creature",
        "human-one",
        vec![
            ("|", vec![1]),
            ("type", b"human".to_vec()),
            ("username", b"alice@example".to_vec()),
            ("publicKey", public_key.as_bytes().to_vec()),
            ("chainId", b"main".to_vec()),
            ("subchainId", b"main".to_vec()),
            ("ownerId", b"free".to_vec()),
            ("balance", 10_i64.to_le_bytes().to_vec()),
        ],
    );
    add_object(
        "Program",
        "program-one",
        vec![
            ("|", vec![1]),
            ("id", b"program-one".to_vec()),
            ("machineId", b"human-one".to_vec()),
            ("runtime", b"wasm".to_vec()),
            ("path", b"/one".to_vec()),
        ],
    );
    add_object(
        "Store",
        "store-one",
        vec![
            ("|", vec![1]),
            ("tag", b"events".to_vec()),
            ("parentId", Vec::new()),
            ("isPublic", vec![0]),
            ("persHist", vec![1]),
            ("memberCount", 1_i32.to_le_bytes().to_vec()),
            ("signalCount", 0_i64.to_le_bytes().to_vec()),
        ],
    );
    for (key, value) in [
        ("link::creatorof::human-one::store-one", "true"),
        ("link::machinePrograms::human-one::program-one", "true"),
        ("index::Creature::username::id::alice@example", "human-one"),
    ] {
        records.push(LegacyPhysicalRecord {
            family: "application-rocksdb-default".to_owned(),
            key: key.as_bytes().to_vec(),
            value: value.as_bytes().to_vec(),
        });
    }
    records
}

fn raw(key: &str, value: &str) -> LegacyPhysicalRecord {
    LegacyPhysicalRecord {
        family: "application-rocksdb-default".to_owned(),
        key: key.as_bytes().to_vec(),
        value: value.as_bytes().to_vec(),
    }
}

#[test]
fn links_and_indexes_are_verified_or_fail_closed() {
    assert!(transform_metadata_fixture(metadata_fixture_subjects()).is_ok());

    // Links that hold primary legacy state are never dropped silently.
    for (key, family) in [
        ("link::NodeIpToHost::10.0.0.1", "link.NodeIpToHost"),
        ("link::ProxyUnknown::x", "link.ProxyUnknown"),
    ] {
        let mut records = metadata_fixture_subjects();
        records.push(raw(key, "7"));
        assert!(matches!(
            transform_metadata_fixture(records),
            Err(LegacyMigrationError::Unmapped { family: found, .. }) if found == family
        ));
    }

    let mut stale_program = metadata_fixture_subjects();
    stale_program.push(raw(
        "link::machinePrograms::someone-else::program-one",
        "true",
    ));
    assert!(transform_metadata_fixture(stale_program).is_err());

    let mut missing_program_link = metadata_fixture_subjects();
    missing_program_link.retain(|record| !record.key.starts_with(b"link::machinePrograms::"));
    assert!(matches!(
        transform_metadata_fixture(missing_program_link),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("no machinePrograms link")
    ));

    // A renamed creature leaves its old username index behind; that stale lookup is
    // observable in legacy, so it must be reconciled before export.
    let mut stale_index = metadata_fixture_subjects();
    stale_index.push(raw(
        "index::Creature::username::id::old-name@example",
        "human-one",
    ));
    assert!(matches!(
        transform_metadata_fixture(stale_index),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("differs")
    ));

    let mut unreviewed_index = metadata_fixture_subjects();
    unreviewed_index.push(raw("index::Program::id::programId::program-one", "x"));
    assert!(matches!(
        transform_metadata_fixture(unreviewed_index),
        Err(LegacyMigrationError::Unmapped { family, .. }) if family == "index"
    ));

    let mut one_sided_email = metadata_fixture_subjects();
    one_sided_email.push(raw("link::UserIdToEmail::human-one", "alice@example.test"));
    assert!(transform_metadata_fixture(one_sided_email.clone()).is_err());
    one_sided_email.push(raw("link::UserEmailToId::alice@example.test", "human-one"));
    let capsules = transform_metadata_fixture(one_sided_email).unwrap();
    assert!(capsules.iter().any(|capsule| matches!(
        &capsule.body,
        Some(CapsuleValue::Object(body))
            if capsule.kind.0 == "core.user"
                && body["email"] == CapsuleValue::Text("alice@example.test".to_owned())
    )));
}

fn metadata_evidence() -> LegacyTransformEvidence {
    LegacyTransformEvidence {
        finance: Some(LegacyFinanceConfig {
            currency: "ASE".to_owned(),
            scale: 2,
        }),
        ..LegacyTransformEvidence::default()
    }
}

fn transform_metadata_fixture(
    records: Vec<LegacyPhysicalRecord>,
) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
    LegacySnapshotGraph::assemble(records)?
        .transform_reviewed_with_evidence(140, &metadata_evidence())
}

#[test]
fn reviewed_metadata_documents_become_structured_subject_bound_capsules() {
    let signup = serde_json::json!({
        "avatar": "a.png",
        "profile": {"bio": "hi", "links": ["x", "y"], "cleared": null},
        "score": 1.5,
        "count": 3
    });
    let mut records = metadata_fixture_subjects();
    legacy_put_json(&mut records, "UserMeta::human-one", "metadata", &signup);
    legacy_put_json(&mut records, "CreatMeta::human-one", "metadata", &signup);
    legacy_put_json(
        &mut records,
        "StoreMeta::store-one",
        "metadata",
        &serde_json::json!({"topic": "events"}),
    );
    legacy_put_json(
        &mut records,
        "ProgMeta::program-one",
        "metadata",
        &serde_json::json!({}),
    );
    records.reverse();
    let capsules = transform_metadata_fixture(records).unwrap();
    assert!(capsules.iter().all(|capsule| capsule.verify().is_ok()));
    let find = |kind: &str| {
        capsules
            .iter()
            .find(|capsule| capsule.kind.0 == kind)
            .unwrap_or_else(|| panic!("missing {kind}"))
    };
    let creature_id = CapsuleId(deterministic_legacy_capsule_id("Creature", b"human-one"));

    let user = find("core.user_metadata");
    let creature = find("core.creature_metadata");
    assert_ne!(user.id, creature.id);
    for capsule in [user, creature] {
        assert_eq!(capsule.owner_scope, OwnerScope::Global);
        assert_eq!(capsule.relationships[0].name, "creature");
        assert_eq!(capsule.relationships[0].target_id, creature_id);
        let Some(CapsuleValue::Object(body)) = &capsule.body else {
            panic!("metadata capsule has no body");
        };
        assert_eq!(
            body["document_path"],
            CapsuleValue::Text("metadata".to_owned())
        );
        assert_eq!(body["entry_count"], CapsuleValue::Integer(4));
        let CapsuleValue::Object(document) = &body["document"] else {
            panic!("document is not structured");
        };
        assert_eq!(document["score"], CapsuleValue::Float(1.5));
        assert_eq!(document["count"], CapsuleValue::Integer(3));
        let CapsuleValue::Object(profile) = &document["profile"] else {
            panic!("nested member is not structured");
        };
        assert_eq!(profile["cleared"], CapsuleValue::Null);
        assert_eq!(
            profile["links"],
            CapsuleValue::Array(vec![
                CapsuleValue::Text("x".to_owned()),
                CapsuleValue::Text("y".to_owned()),
            ])
        );
        assert_eq!(
            body["content_digest"],
            CapsuleValue::Bytes(legacy_document_digest(&body["document"]).unwrap())
        );
    }
    let user_body = user.body.as_ref().unwrap();
    let creature_body = creature.body.as_ref().unwrap();
    assert_eq!(user_body, creature_body);

    let store = find("core.store_metadata");
    assert_eq!(store.owner_scope, OwnerScope::Creature(creature_id.0));
    assert_eq!(store.relationships[0].name, "store");
    assert_eq!(
        store.relationships[0].target_id,
        CapsuleId(deterministic_legacy_capsule_id("Store", b"store-one"))
    );

    let program = find("core.program_metadata");
    assert_eq!(program.owner_scope, OwnerScope::Creature(creature_id.0));
    assert_eq!(
        program.relationships[0].target_id,
        CapsuleId(deterministic_legacy_capsule_id("Program", b"program-one"))
    );
    assert!(matches!(
        &program.body,
        Some(CapsuleValue::Object(body))
            if body["entry_count"] == CapsuleValue::Integer(0)
                && body["document"] == CapsuleValue::Object(BTreeMap::new())
    ));
}

#[test]
fn divergent_legacy_splats_fail_closed() {
    let document = serde_json::json!({"a": 1, "nested": {"b": true}});
    let base = || {
        let mut records = metadata_fixture_subjects();
        legacy_put_json(&mut records, "ProgMeta::program-one", "metadata", &document);
        records
    };
    assert!(transform_metadata_fixture(base()).is_ok());

    // A merged `null` or a non-merging rewrite leaves a stale splat behind.
    let mut stale = base();
    stale.push(LegacyPhysicalRecord {
        family: "application-rocksdb-default".to_owned(),
        key: b"json::ProgMeta::program-one::metadata.removed".to_vec(),
        value: b"\"old\"".to_vec(),
    });
    assert!(matches!(
        transform_metadata_fixture(stale),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("is stale")
    ));

    let mut unequal = base();
    for record in &mut unequal {
        if record.key == b"json::ProgMeta::program-one::metadata.nested.b" {
            record.value = b"false".to_vec();
        }
    }
    assert!(matches!(
        transform_metadata_fixture(unequal),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("disagrees")
    ));

    let mut missing = base();
    missing.retain(|record| record.key != b"json::ProgMeta::program-one::metadata.a");
    assert!(matches!(
        transform_metadata_fixture(missing),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("omits")
    ));

    let mut rootless = base();
    rootless.retain(|record| record.key != b"json::ProgMeta::program-one::metadata");
    assert!(transform_metadata_fixture(rootless).is_err());

    let mut foreign_path = base();
    foreign_path.push(LegacyPhysicalRecord {
        family: "application-rocksdb-default".to_owned(),
        key: b"json::ProgMeta::program-one::other".to_vec(),
        value: b"{}".to_vec(),
    });
    assert!(matches!(
        transform_metadata_fixture(foreign_path),
        Err(LegacyMigrationError::Unmapped { family, .. }) if family == "json-document-path"
    ));
}

#[test]
fn dangling_unreviewed_and_out_of_range_documents_fail_closed() {
    // Legacy store deletion never removes `StoreMeta` (it deletes the
    // unprefixed `Json::StoreMeta::{id}::metadata` key), so orphans exist.
    let mut orphan = metadata_fixture_subjects();
    legacy_put_json(
        &mut orphan,
        "StoreMeta::deleted-store",
        "metadata",
        &serde_json::json!({"topic": "gone"}),
    );
    assert!(matches!(
        transform_metadata_fixture(orphan),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("deleted-store")
    ));

    let mut unreviewed = metadata_fixture_subjects();
    legacy_put_json(
        &mut unreviewed,
        "Json::Unreviewed::grant-1",
        "state",
        &serde_json::json!({"port": 8080}),
    );
    assert!(matches!(
        transform_metadata_fixture(unreviewed),
        Err(LegacyMigrationError::Unmapped { family, key })
            if family == "json-document" && key == "Json::Unreviewed::grant-1"
    ));

    let mut scalar_root = metadata_fixture_subjects();
    scalar_root.push(LegacyPhysicalRecord {
        family: "application-rocksdb-default".to_owned(),
        key: b"json::ProgMeta::program-one::metadata".to_vec(),
        value: b"[1]".to_vec(),
    });
    assert!(transform_metadata_fixture(scalar_root).is_err());

    let mut overflow = metadata_fixture_subjects();
    overflow.push(LegacyPhysicalRecord {
        family: "application-rocksdb-default".to_owned(),
        key: b"json::ProgMeta::program-one::metadata".to_vec(),
        value: b"{\"n\":18446744073709551615}".to_vec(),
    });
    overflow.push(LegacyPhysicalRecord {
        family: "application-rocksdb-default".to_owned(),
        key: b"json::ProgMeta::program-one::metadata.n".to_vec(),
        value: b"18446744073709551615".to_vec(),
    });
    assert!(matches!(
        transform_metadata_fixture(overflow),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("outside the capsule range")
    ));

    let mut duplicate = metadata_fixture_subjects();
    legacy_put_json(
        &mut duplicate,
        "ProgMeta::program-one",
        "metadata",
        &serde_json::json!({}),
    );
    legacy_put_json(
        &mut duplicate,
        "ProgMeta::program-one",
        "metadata",
        &serde_json::json!({}),
    );
    assert!(LegacySnapshotGraph::assemble(duplicate).is_err());
}

#[test]
fn rocksdb_source_is_read_only_and_enforces_snapshot_bounds() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "aseman-storage-legacy-test-{}-{unique}",
        std::process::id()
    ));
    assert!(!path.exists());
    {
        let database = DB::open_default(&path).unwrap();
        database.put(b"known", b"alice").unwrap();
    }
    let source = RocksDbLegacySource::open_read_only(&path, "snapshot-one").unwrap();
    let snapshot = source.read_snapshot(1, 64).unwrap();
    assert_eq!(snapshot.snapshot_id, "snapshot-one");
    assert_eq!(snapshot.records.len(), 1);
    assert!(source.read_snapshot(1, 4).is_err());
    drop(source);
    DB::destroy(&Options::default(), &path).unwrap();
    assert!(!path.exists());
}

/// A balanced legacy finance epoch written the way `creature/finance.rs` writes it.
fn finance_fixture() -> Vec<LegacyPhysicalRecord> {
    let mut records = metadata_fixture_subjects();
    let public_key = test_rsa_key_pair(1).1;
    for (column, value) in [
        ("|", vec![1]),
        ("type", b"human".to_vec()),
        ("username", b"bob@example".to_vec()),
        ("publicKey", public_key.as_bytes().to_vec()),
        ("chainId", b"main".to_vec()),
        ("subchainId", b"main".to_vec()),
        ("ownerId", b"free".to_vec()),
        ("balance", 100_i64.to_le_bytes().to_vec()),
    ] {
        records.push(LegacyPhysicalRecord {
            family: "application-rocksdb-default".to_owned(),
            key: format!("obj::Creature::human-two::{column}").into_bytes(),
            value,
        });
    }
    records.push(raw(
        "index::Creature::username::id::bob@example",
        "human-two",
    ));
    let documents = [
        (
            "Json::FinanceHold::hold-1",
            "hold",
            serde_json::json!({
                "holdId": "hold-1", "payerUserId": "human-one", "status": "settled",
                "maxAmount": 100, "remainingAmount": 0, "actualAmount": 60,
                "refundedAmount": 40, "requestHash": "h1",
                "settlementLines": [{"userId": "human-two", "amount": 60}]
            }),
        ),
        (
            "Json::FinanceHold::hold-2",
            "hold",
            serde_json::json!({
                "holdId": "hold-2", "payerUserId": "human-one", "status": "open",
                "maxAmount": 50, "remainingAmount": 50, "requestHash": "h2"
            }),
        ),
        (
            "Json::FinancePool::pool-1",
            "pool",
            serde_json::json!({
                "payerUserId": "human-one", "status": "open", "maxAmount": 30,
                "remaining": 10, "reserved": 20, "spent": 0, "refunded": 0
            }),
        ),
        (
            "Json::FinancePoolReservation::run-1",
            "reservation",
            serde_json::json!({
                "payerUserId": "human-one", "poolId": "pool-1",
                "status": "reserved", "amount": 20
            }),
        ),
        (
            "Json::FinanceJournal::journal-1",
            "entry",
            serde_json::json!({
                "journalId": "journal-1", "kind": "hold.settled", "holdId": "hold-1",
                "payerUserId": "human-one", "createdAt": 5,
                "payload": {"entries": [{"account": "wallet:human-one:held", "amount": 60}]}
            }),
        ),
        (
            "Json::Creature::human-one",
            "lockedTokens.lock-a",
            serde_json::json!({
                "type": "pay", "amount": 10, "remainingAmount": 10, "userId": "human-two",
                "steps": [{"amount": 10, "unlockAt": 5}]
            }),
        ),
        (
            "Json::VmBilling::vm-1",
            "payment",
            serde_json::json!({"payerUserId": "human-one", "holdId": "hold-2"}),
        ),
        (
            "Json::CreatureNamespace::billing",
            "current",
            serde_json::json!({"version": 1, "catalogHash": "abc"}),
        ),
    ];
    for (key, path, document) in documents {
        legacy_put_json(&mut records, key, path, &document);
    }
    for (key, value) in [
        ("link::FinanceHeld::human-one", "80"),
        ("link::FinanceSpent::human-one", "60"),
        ("link::FinanceEarned::human-two", "60"),
        ("link::FinanceWithdrawable::human-two", "60"),
        ("link::FinanceDebt::human-one", "5"),
        ("link::VmBilling::vm-1", "true"),
        ("link::FinanceHoldRequest::human-one::idem-1", "hold-1|h1"),
        ("link::FinanceSettlement::authority-1::settle-1", "hold-1"),
        ("link::FinancePoolOpen::human-one::open-1", "pool-1"),
        ("link::MintApplied::mint-key-1", "human-two:25:journal-1"),
        ("link::FinancePoolByUser::human-one", "pool-1"),
        (
            "link::FinanceHoldByPayer::human-one::00000000000000000005::hold-1",
            "hold-1",
        ),
        (
            "link::FinanceJournalByUser::human-two::00000000000000000005::journal-1",
            "journal-1",
        ),
    ] {
        records.push(raw(key, value));
    }
    records
}

fn finance_capsules(
    records: Vec<LegacyPhysicalRecord>,
) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
    Ok(transform_metadata_fixture(records)?
        .into_iter()
        .filter(|capsule| capsule.kind.0 == LEGACY_FINANCE_KIND)
        .collect())
}

fn finance_body(capsule: &CapsuleEnvelope) -> &BTreeMap<String, CapsuleValue> {
    match &capsule.body {
        Some(CapsuleValue::Object(body)) => body,
        _ => panic!("finance capsule has no body"),
    }
}

#[test]
fn reconciled_finance_epoch_exports_authorities_and_drops_projections() {
    let capsules = finance_capsules(finance_fixture()).unwrap();
    assert!(capsules.iter().all(|capsule| capsule.verify().is_ok()
        && capsule.storage_class == StorageClass::Finance
        && capsule.owner_scope == OwnerScope::Global));
    let mut families = capsules
        .iter()
        .map(|capsule| {
            let body = finance_body(capsule);
            let (CapsuleValue::Text(family), CapsuleValue::Text(key)) =
                (&body["record_family"], &body["legacy_key"])
            else {
                panic!("record identity is not text");
            };
            (family.clone(), key.clone())
        })
        .collect::<Vec<_>>();
    families.sort();
    let expected = [
        ("billing_namespace", "current"),
        ("debt_counter", "human-one"),
        ("hold", "hold-1"),
        ("hold", "hold-2"),
        ("hold_request", "human-one::idem-1"),
        ("hold_settlement", "authority-1::settle-1"),
        ("journal_entry", "journal-1"),
        ("mint_applied", "mint-key-1"),
        ("pool", "pool-1"),
        ("pool_open", "human-one::open-1"),
        ("pool_reservation", "run-1"),
        ("token_lock", "human-one::lock-a"),
        ("vm_billing", "vm-1"),
        ("withdrawable_counter", "human-two"),
    ]
    .map(|(family, key)| (family.to_owned(), key.to_owned()));
    // Derived counters and listing links are verified, never exported.
    assert_eq!(families, expected);

    let debt = capsules
        .iter()
        .find(|capsule| {
            finance_body(capsule)["record_family"] == CapsuleValue::Text("debt_counter".to_owned())
        })
        .unwrap();
    let body = finance_body(debt);
    assert_eq!(body["currency"], CapsuleValue::Text("ASE".to_owned()));
    assert_eq!(body["scale"], CapsuleValue::Integer(2));
    assert_eq!(
        body["document"],
        CapsuleValue::Object(BTreeMap::from([(
            "amount".to_owned(),
            CapsuleValue::Integer(5)
        )]))
    );
    assert_eq!(
        body["content_digest"],
        CapsuleValue::Bytes(legacy_document_digest(&body["document"]).unwrap())
    );
    // Re-running the export yields byte-identical capsules.
    assert_eq!(finance_capsules(finance_fixture()).unwrap(), capsules);
}

fn replace_json(
    records: &mut [LegacyPhysicalRecord],
    key: &str,
    path: &str,
    document: serde_json::Value,
) {
    let prefix = format!("json::{key}::{path}");
    let mut rewritten = Vec::new();
    legacy_put_json(&mut rewritten, key, path, &document);
    for record in records.iter_mut() {
        if record.key.starts_with(prefix.as_bytes()) {
            let replacement = rewritten
                .iter()
                .find(|candidate| candidate.key == record.key)
                .unwrap_or_else(|| {
                    panic!(
                        "fixture path {} vanished",
                        String::from_utf8_lossy(&record.key)
                    )
                });
            record.value = replacement.value.clone();
        }
    }
}

fn set_link(records: &mut Vec<LegacyPhysicalRecord>, key: &str, value: &str) {
    records.retain(|record| record.key != key.as_bytes());
    records.push(raw(key, value));
}

fn assert_finance_rejected(records: Vec<LegacyPhysicalRecord>, expected: &str) {
    match finance_capsules(records) {
        Err(
            LegacyMigrationError::Invalid(message)
            | LegacyMigrationError::Unmapped {
                family: message, ..
            },
        ) => {
            assert!(
                message.contains(expected),
                "{message} does not mention {expected}"
            );
        }
        other => panic!("expected rejection mentioning {expected}, got {other:?}"),
    }
}

#[test]
fn finance_reconciliation_and_references_fail_closed() {
    let mut drift = finance_fixture();
    set_link(&mut drift, "link::FinanceHeld::human-one", "81");
    assert_finance_rejected(drift, "held.mismatch");

    let mut missing_counter = finance_fixture();
    missing_counter.retain(|record| record.key != b"link::FinanceEarned::human-two");
    assert_finance_rejected(missing_counter, "earned.mismatch");

    let mut lines = finance_fixture();
    replace_json(
        &mut lines,
        "Json::FinanceHold::hold-1",
        "hold",
        serde_json::json!({
            "holdId": "hold-1", "payerUserId": "human-one", "status": "settled",
            "maxAmount": 100, "remainingAmount": 0, "actualAmount": 60,
            "refundedAmount": 40, "requestHash": "h1",
            "settlementLines": [{"userId": "human-two", "amount": 59}]
        }),
    );
    assert_finance_rejected(lines, "settlement.lines_mismatch");

    let mut pool = finance_fixture();
    replace_json(
        &mut pool,
        "Json::FinancePool::pool-1",
        "pool",
        serde_json::json!({
            "payerUserId": "human-one", "status": "open", "maxAmount": 31,
            "remaining": 10, "reserved": 20, "spent": 0, "refunded": 0
        }),
    );
    assert_finance_rejected(pool, "pool.balance_mismatch");

    let mut float_money = finance_fixture();
    replace_json(
        &mut float_money,
        "Json::FinanceHold::hold-2",
        "hold",
        serde_json::json!({
            "holdId": "hold-2", "payerUserId": "human-one", "status": "open",
            "maxAmount": 50.0, "remainingAmount": 50, "requestHash": "h2"
        }),
    );
    assert_finance_rejected(float_money, "amount.not_integer");

    let mut unbacked = finance_fixture();
    set_link(&mut unbacked, "link::FinanceWithdrawable::human-one", "5");
    assert_finance_rejected(unbacked, "withdrawable.unbacked_total");

    let mut over_balance = finance_fixture();
    set_link(
        &mut over_balance,
        "link::FinanceWithdrawable::human-two",
        "101",
    );
    assert_finance_rejected(over_balance, "withdrawable.invalid");

    let mut dangling_marker = finance_fixture();
    set_link(
        &mut dangling_marker,
        "link::FinanceRun::authority-1::run-9",
        "hold-9",
    );
    assert_finance_rejected(dangling_marker, "names no matching hold");

    let mut forged_request = finance_fixture();
    set_link(
        &mut forged_request,
        "link::FinanceHoldRequest::human-one::idem-1",
        "hold-1|other",
    );
    assert_finance_rejected(forged_request, "names no matching hold");

    let mut unjournaled_mint = finance_fixture();
    set_link(
        &mut unjournaled_mint,
        "link::MintApplied::mint-key-1",
        "human-two:25:journal-9",
    );
    assert_finance_rejected(unjournaled_mint, "names no matching journal_entry");

    let mut wrong_party = finance_fixture();
    set_link(
        &mut wrong_party,
        "link::FinanceHoldByPayer::human-two::00000000000000000005::hold-1",
        "hold-1",
    );
    assert_finance_rejected(wrong_party, "diverges from its record");

    let mut negative = finance_fixture();
    set_link(&mut negative, "link::FinanceDebt::human-one", "-1");
    assert_finance_rejected(negative, "nonnegative integer");

    let mut foreign_root = finance_fixture();
    legacy_put_json(
        &mut foreign_root,
        "Json::FinanceHold::hold-1",
        "annotations",
        &serde_json::json!({}),
    );
    assert_finance_rejected(foreign_root, "json-document-path");

    let mut market_unknown = finance_fixture();
    legacy_put_json(
        &mut market_unknown,
        "Json::CreatureNamespace::market",
        "widgets",
        &serde_json::json!({}),
    );
    assert_finance_rejected(market_unknown, "json-document-path");
}

#[test]
fn finance_epoch_requires_explicit_currency_and_scale() {
    let graph = LegacySnapshotGraph::assemble(finance_fixture()).unwrap();
    assert!(matches!(
        graph.transform_reviewed_with_evidence(
            140,
            &LegacyTransformEvidence::default(),
        ),
        Err(LegacyMigrationError::Unmapped { family, .. }) if family.contains("finance_config")
    ));
}

fn membership_fixture() -> Vec<LegacyPhysicalRecord> {
    let mut records = metadata_fixture_subjects();
    for (key, value) in [
        ("link::onaccess::store-one::human-one", "read,signal,manage"),
        ("link::hasaccess::human-one::store-one", "true"),
        ("link::onaccess::store-one::program-one", "signal,read"),
        ("link::hasaccess::program-one::store-one", "true"),
        ("link::onaccess::store-one::7@remote.example", "true"),
        ("link::hasaccess::7@remote.example::store-one", "true"),
    ] {
        records.push(raw(key, value));
    }
    records
}

fn membership_capsules(
    records: Vec<LegacyPhysicalRecord>,
    local_origins: &[&str],
) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
    let evidence = LegacyTransformEvidence {
        local_origins: local_origins
            .iter()
            .map(|origin| (*origin).to_owned())
            .collect(),
        ..metadata_evidence()
    };
    Ok(LegacySnapshotGraph::assemble(records)?
        .transform_reviewed_with_evidence(140, &evidence)?
        .into_iter()
        .filter(|capsule| capsule.kind.0 == "core.store_membership")
        .collect())
}

#[test]
fn memberships_keep_exact_permissions_and_typed_principals() {
    let capsules = membership_capsules(membership_fixture(), &["global", "local.example"]).unwrap();
    assert_eq!(capsules.len(), 3);
    let creature = CapsuleId(deterministic_legacy_capsule_id("Creature", b"human-one"));
    let store = CapsuleId(deterministic_legacy_capsule_id("Store", b"store-one"));
    let mut seen = BTreeMap::new();
    for capsule in &capsules {
        assert!(capsule.verify().is_ok());
        assert_eq!(capsule.owner_scope, OwnerScope::Creature(creature.0));
        assert_eq!(capsule.relationships[0].name, "store");
        assert_eq!(capsule.relationships[0].target_id, store);
        let Some(CapsuleValue::Object(body)) = &capsule.body else {
            panic!("membership has no body");
        };
        assert_eq!(body["joined_at_micros"], CapsuleValue::Integer(0));
        let (CapsuleValue::Text(kind), CapsuleValue::Text(member), CapsuleValue::Text(permissions)) = (
            &body["member_kind"],
            &body["member_ref"],
            &body["permissions"],
        ) else {
            panic!("membership fields are not text");
        };
        let local = capsule
            .relationships
            .get(1)
            .map(|relationship| relationship.name.clone());
        seen.insert(member.clone(), (kind.clone(), permissions.clone(), local));
    }
    assert_eq!(
        seen,
        BTreeMap::from([
            (
                "human-one".to_owned(),
                (
                    "creature".to_owned(),
                    "read,signal,manage".to_owned(),
                    Some("creature".to_owned())
                ),
            ),
            (
                "program-one".to_owned(),
                (
                    "program".to_owned(),
                    "read,signal".to_owned(),
                    Some("program".to_owned())
                ),
            ),
            (
                "7@remote.example".to_owned(),
                ("remote_principal".to_owned(), String::new(), None),
            ),
        ])
    );
}

#[test]
fn membership_pairs_permissions_and_locality_fail_closed() {
    let origins = ["global", "local.example"];
    let mut one_sided = membership_fixture();
    one_sided.retain(|record| record.key != b"link::hasaccess::program-one::store-one");
    assert!(matches!(
        membership_capsules(one_sided, &origins),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("onaccess without hasaccess")
    ));

    let mut flag_only = membership_fixture();
    flag_only.retain(|record| record.key != b"link::onaccess::store-one::program-one");
    assert!(matches!(
        membership_capsules(flag_only, &origins),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("hasaccess without onaccess")
    ));

    for (value, expected) in [("read,teleport", "unknown token"), ("read,read", "repeats")] {
        let mut records = membership_fixture();
        set_link(
            &mut records,
            "link::onaccess::store-one::program-one",
            value,
        );
        assert!(matches!(
            membership_capsules(records, &origins),
            Err(LegacyMigrationError::Invalid(message)) if message.contains(expected)
        ));
    }

    let mut not_true = membership_fixture();
    set_link(
        &mut not_true,
        "link::hasaccess::program-one::store-one",
        "yes",
    );
    assert!(membership_capsules(not_true, &origins).is_err());

    // A local-origin identity with no local row is dangling, never "remote".
    let mut dangling = membership_fixture();
    dangling.push(raw("link::onaccess::store-one::9@local.example", "read"));
    dangling.push(raw("link::hasaccess::9@local.example::store-one", "true"));
    assert!(matches!(
        membership_capsules(dangling, &origins),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("local origin")
    ));

    assert!(matches!(
        membership_capsules(membership_fixture(), &[]),
        Err(LegacyMigrationError::Unmapped { family, .. }) if family == "StoreMembership.local_origins"
    ));

    let mut unqualified = membership_fixture();
    unqualified.push(raw("link::onaccess::store-one::ghost", "read"));
    unqualified.push(raw("link::hasaccess::ghost::store-one", "true"));
    assert!(membership_capsules(unqualified, &origins).is_err());

    let mut missing_store = membership_fixture();
    missing_store.push(raw("link::onaccess::store-gone::human-one", "read"));
    missing_store.push(raw("link::hasaccess::human-one::store-gone", "true"));
    assert!(membership_capsules(missing_store, &origins).is_err());
}

#[test]
fn custodial_private_keys_are_verified_and_never_exported() {
    let (private, _) = test_rsa_key_pair(0);
    let mut records = metadata_fixture_subjects();
    records.push(raw("link::UserPrivateKey::human-one", &private));
    let graph = LegacySnapshotGraph::assemble(records.clone()).unwrap();
    assert_eq!(graph.verify_legacy_custodial_keys().unwrap(), 1);
    let capsules = transform_metadata_fixture(records).unwrap();
    // No capsule, digest input, or field carries the key material.
    let marker = private.lines().nth(1).unwrap().as_bytes().to_vec();
    for capsule in &capsules {
        let encoded = capsule.canonical_bytes().unwrap();
        assert!(
            !encoded
                .windows(marker.len())
                .any(|window| window == marker.as_slice())
        );
    }

    // A key for another identity would hand out the wrong credential at login.
    let (foreign, _) = test_rsa_key_pair(2);
    let mut mismatched = metadata_fixture_subjects();
    mismatched.push(raw("link::UserPrivateKey::human-one", &foreign));
    match transform_metadata_fixture(mismatched) {
        Err(LegacyMigrationError::Invalid(message)) => {
            assert!(message.contains("does not match"));
            assert!(!message.contains("PRIVATE KEY"));
        }
        other => panic!("expected mismatch rejection, got {other:?}"),
    }

    let mut garbage = metadata_fixture_subjects();
    garbage.push(raw("link::UserPrivateKey::human-one", "not a key"));
    assert!(matches!(
        transform_metadata_fixture(garbage),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("not a PKCS#8") && !message.contains("not a key")
    ));

    let mut orphan = metadata_fixture_subjects();
    orphan.push(raw("link::UserPrivateKey::human-gone", &private));
    assert!(transform_metadata_fixture(orphan).is_err());
}

fn raw_bytes(key: &str, value: Vec<u8>) -> LegacyPhysicalRecord {
    LegacyPhysicalRecord {
        family: "application-rocksdb-default".to_owned(),
        key: key.as_bytes().to_vec(),
        value,
    }
}

#[test]
fn operational_raw_keys_are_verified_and_never_exported() {
    let mut records = metadata_fixture_subjects();
    // Mint a global-origin program so the global counter has a floor.
    for (column, value) in [
        ("|", vec![1]),
        ("id", b"12@global".to_vec()),
        ("machineId", b"human-one".to_vec()),
        ("runtime", b"wasm".to_vec()),
        ("path", b"/global".to_vec()),
    ] {
        records.push(raw_bytes(
            &format!("obj::Program::12@global::{column}"),
            value,
        ));
    }
    records.push(raw("link::machinePrograms::human-one::12@global", "true"));
    records.push(raw_bytes("globalIdCounter", 12_i64.to_be_bytes().to_vec()));
    records.push(raw_bytes("localIdCounter", 3_i64.to_be_bytes().to_vec()));
    for (key, value) in [
        ("chainCallback::human-one_tag|>tail-1", vec![1]),
        (
            "chainCallback::human-one_tag|tail-1::machineId",
            b"human-one".to_vec(),
        ),
        (
            "chainCallback::human-one_tag|tail-1::storeId",
            b"store-one".to_vec(),
        ),
        (
            "chainCallback::human-one_tag|tail-1::attachment",
            b"{}".to_vec(),
        ),
        (
            "chainCallback::human-one_tag::targetCount",
            2_u32.to_be_bytes().to_vec(),
        ),
        (
            "chainCallback::human-one_tag::tempCount",
            0_u32.to_be_bytes().to_vec(),
        ),
    ] {
        records.push(raw_bytes(key, value));
    }
    let capsules = transform_metadata_fixture(records.clone()).unwrap();
    assert_eq!(
        capsules
            .iter()
            .filter(|capsule| capsule.kind.0 == "core.program")
            .count(),
        2
    );

    let mut behind = records.clone();
    behind.retain(|record| record.key != b"globalIdCounter");
    behind.push(raw_bytes("globalIdCounter", 11_i64.to_be_bytes().to_vec()));
    assert!(matches!(
        transform_metadata_fixture(behind),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("mint duplicates")
    ));

    let mut short = records.clone();
    short.retain(|record| record.key != b"localIdCounter");
    short.push(raw_bytes("localIdCounter", vec![0, 1]));
    assert!(transform_metadata_fixture(short).is_err());

    let mut odd_callback = records.clone();
    odd_callback.push(raw_bytes("chainCallback::human-one_tag::surprise", vec![9]));
    assert!(matches!(
        transform_metadata_fixture(odd_callback),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("dead-state shape")
    ));

    let mut superuser = records;
    superuser.push(raw("god::human-one", "true"));
    assert!(matches!(
        transform_metadata_fixture(superuser),
        Err(LegacyMigrationError::Unmapped { family, .. }) if family == "raw.god"
    ));
}

#[test]
fn guest_kv_moves_into_its_machine_creature_and_foreign_prefixes_fail() {
    // `vm_db_op`/`VmTrxBuffer::commit` persist guest pairs with `put_link`.
    let mut records = metadata_fixture_subjects();
    records.push(raw("link::human-one::profile/name", "alice"));
    records.push(raw("link::human-one::", "empty-key"));
    let capsules = transform_metadata_fixture(records)
        .unwrap()
        .into_iter()
        .filter(|capsule| capsule.kind.0 == LEGACY_GUEST_KV_KIND)
        .collect::<Vec<_>>();
    assert_eq!(capsules.len(), 2);
    let owner = deterministic_legacy_capsule_id("Creature", b"human-one");
    for capsule in &capsules {
        assert!(capsule.verify().is_ok());
        assert_eq!(capsule.storage_class, StorageClass::GuestData);
        assert_eq!(capsule.owner_scope, OwnerScope::Creature(owner));
        assert!(capsule.relationships.is_empty());
    }
    assert!(capsules.iter().any(|capsule| matches!(
        &capsule.body,
        Some(CapsuleValue::Object(body))
            if body["key"] == CapsuleValue::Text("profile/name".to_owned())
                && body["value"] == CapsuleValue::Text("alice".to_owned())
    )));

    // A link whose family is not a local creature is not guest data.
    let mut foreign = metadata_fixture_subjects();
    foreign.push(raw("link::someone@else::key", "value"));
    assert!(matches!(
        transform_metadata_fixture(foreign),
        Err(LegacyMigrationError::Unmapped { family, .. }) if family == "link.someone@else"
    ));

    // No legacy writer produces raw `{machine}::{key}` records; they stay unmapped.
    let mut raw_guest = metadata_fixture_subjects();
    raw_guest.push(raw("human-one::key", "value"));
    assert!(matches!(
        LegacySnapshotGraph::assemble(raw_guest),
        Err(LegacyMigrationError::Unmapped { .. })
    ));

    let mut binary = metadata_fixture_subjects();
    binary.push(raw_bytes("link::human-one::blob", vec![0xff, 0xfe]));
    assert!(matches!(
        transform_metadata_fixture(binary),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("not UTF-8")
    ));
}

#[test]
fn creature_type_registry_exports_specs_and_verifies_flags() {
    let spec = serde_json::json!({
        "initialBalance": 0, "customFields": [], "desc": "A non-human being that can own programs."
    });
    let mut records = metadata_fixture_subjects();
    legacy_put_json(&mut records, "Json::CreatureType::machine", "spec", &spec);
    records.push(raw("link::CreatureTypeExists::machine", "true"));
    let types = transform_metadata_fixture(records.clone())
        .unwrap()
        .into_iter()
        .filter(|capsule| capsule.kind.0 == "core.creature_type")
        .collect::<Vec<_>>();
    assert_eq!(types.len(), 1);
    assert!(types[0].verify().is_ok());
    assert!(matches!(
        &types[0].body,
        Some(CapsuleValue::Object(body))
            if body["type_name"] == CapsuleValue::Text("machine".to_owned())
                && body["entry_count"] == CapsuleValue::Integer(3)
    ));

    let mut unflagged = records.clone();
    unflagged.retain(|record| record.key != b"link::CreatureTypeExists::machine");
    assert!(matches!(
        transform_metadata_fixture(unflagged),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("no CreatureTypeExists")
    ));

    let mut phantom = records;
    phantom.push(raw("link::CreatureTypeExists::robot", "true"));
    assert!(matches!(
        transform_metadata_fixture(phantom),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("diverges")
    ));
}

#[test]
fn applet_db_guest_storage_resolves_creature_or_program_owner() {
    let mut records = metadata_fixture_subjects();
    records.push(raw("link::AppletDb::human-one::program-one::counter", "3"));
    records.push(raw("link::AppletDb::program-one::cache::a", "x"));
    records.push(raw("link::human-one::counter", "legacy-dbop"));
    let capsules = transform_metadata_fixture(records)
        .unwrap()
        .into_iter()
        .filter(|capsule| capsule.kind.0 == LEGACY_GUEST_KV_KIND)
        .collect::<Vec<_>>();
    assert_eq!(capsules.len(), 3);
    let owner = deterministic_legacy_capsule_id("Creature", b"human-one");
    let mut seen = capsules
        .iter()
        .map(|capsule| {
            assert_eq!(capsule.owner_scope, OwnerScope::Creature(owner));
            let Some(CapsuleValue::Object(body)) = &capsule.body else {
                panic!("guest capsule has no body");
            };
            (body["namespace"].clone(), body["key"].clone())
        })
        .collect::<Vec<_>>();
    seen.sort_by_key(|(namespace, key)| format!("{namespace:?}{key:?}"));
    let text = |value: &str| CapsuleValue::Text(value.to_owned());
    assert_eq!(
        seen,
        vec![
            (text("applet_db"), text("human-one::program-one::counter")),
            (text("applet_db"), text("program-one::cache::a")),
            (text("dbop"), text("counter")),
        ]
    );

    let mut orphan = metadata_fixture_subjects();
    orphan.push(raw("link::AppletDb::nobody::key", "x"));
    assert!(matches!(
        transform_metadata_fixture(orphan),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("names no local creature or program")
    ));
}

#[test]
fn openraft_checkpoint_is_read_only_digested_and_replica_checked() {
    let state = br#"{"last_applied":{"leader_id":{"term":3,"node_id":1},"index":42},
        "membership":{"log_id":null,"membership":{"configs":[[1,2,3]],"nodes":{}}},
        "shared_config":{"telemetryInterval":"30s","region":"eu"}}"#;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "aseman-openraft-test-{}-{unique}",
        std::process::id()
    ));
    {
        let mut options = Options::default();
        options.create_if_missing(true);
        options.create_missing_column_families(true);
        let database = DB::open_cf(&options, &path, ["meta", "logs", "sm"]).unwrap();
        database
            .put_cf(database.cf_handle("sm").unwrap(), "state", state)
            .unwrap();
    }
    let replica = LegacyClusterCheckpoint::read_only(&path).unwrap();
    assert_eq!(
        replica.operator_knobs,
        BTreeSet::from(["region".to_owned(), "telemetryInterval".to_owned()])
    );
    let reparsed = LegacyClusterCheckpoint::from_state_json(Some(state)).unwrap();
    assert_eq!(replica, reparsed);
    assert_eq!(
        verify_legacy_cluster_replicas(&[replica.clone(), reparsed]).unwrap(),
        replica.digest
    );
    DB::destroy(&Options::default(), &path).unwrap();

    let lagging = LegacyClusterCheckpoint::from_state_json(Some(
        br#"{"last_applied":{"leader_id":{"term":3,"node_id":1},"index":41},
        "membership":{"log_id":null,"membership":{"configs":[[1,2,3]],"nodes":{}}},
        "shared_config":{"telemetryInterval":"30s","region":"eu"}}"#,
    ))
    .unwrap();
    assert!(matches!(
        verify_legacy_cluster_replicas(&[replica, lagging]),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("replica 1 diverges")
    ));

    let default = LegacyClusterCheckpoint::from_state_json(None).unwrap();
    assert!(default.operator_knobs.is_empty());
    assert!(LegacyClusterCheckpoint::from_state_json(Some(br#"{"last_applied":null}"#)).is_err());
    assert!(
        LegacyClusterCheckpoint::from_state_json(Some(
            br#"{"last_applied":null,"membership":null,"shared_config":{},"extra":1}"#
        ))
        .is_err()
    );
    assert!(verify_legacy_cluster_replicas(&[]).is_err());
}

#[test]
fn observed_vm_runtime_is_inventoried_for_the_vmm_and_never_exported() {
    let mut records = metadata_fixture_subjects();
    for (key, value) in [
        ("link::VmInstance::program-one::main::vm-9", "true"),
        ("link::VmStatus::vm-9", "running"),
        ("link::VmStartedAt::vm-9", "1700000000000"),
        ("link::VmOwnerProgram::vm-9", "program-one"),
        (
            "link::VmContainerName::program-one::main::vm-9",
            "caspar-vm-9",
        ),
        ("link::VmTerminal::human-one::vm-9::human-one", "true"),
        ("link::VmBuilds::vm-9::build-1", "true"),
        ("link::ModalVolume::vm-9", "vo-123"),
        ("link::ModalApp::human-one", "ap-1"),
        ("link::ProxyCorrExpiry::corr-1", "1700000060000"),
        ("link::vmDistribution::program-one", "local"),
        ("link::vmDistribution::program-one::main", "cluster"),
    ] {
        records.push(raw(key, value));
    }
    legacy_put_json(
        &mut records,
        "Json::ProxyCorrelation::corr-1",
        "record",
        &serde_json::json!({"target": "program-one"}),
    );
    let graph = LegacySnapshotGraph::assemble(records.clone()).unwrap();
    let inventory = graph.legacy_vmm_handoff_inventory().unwrap();
    assert_eq!(inventory["VmInstance"], 1);
    assert_eq!(inventory["ModalVolume"], 1);
    assert_eq!(inventory["Json::ProxyCorrelation"], 1);
    let capsules = transform_metadata_fixture(records.clone()).unwrap();
    // Observed runtime never becomes a capsule, and no workload is fabricated.
    assert!(
        capsules
            .iter()
            .all(|capsule| capsule.kind.0 != "core.workload")
    );
    assert_eq!(
        capsules.len(),
        transform_metadata_fixture(metadata_fixture_subjects())
            .unwrap()
            .len()
    );

    for (key, value) in [
        ("link::VmStatus::vm-9", "paused"),
        ("link::VmStartedAt::vm-9", "soon"),
        ("link::VmInstance::program-one::vm-9", "true"),
        ("link::vmDistribution::program-one", "everywhere"),
    ] {
        let mut bad = records.clone();
        set_link(&mut bad, key, value);
        assert!(
            transform_metadata_fixture(bad).is_err(),
            "{key}={value} should fail closed"
        );
    }
}

#[test]
fn vm_billing_flag_must_match_its_payment_record() {
    let mut orphan_flag = finance_fixture();
    orphan_flag.push(raw("link::VmBilling::vm-404", "true"));
    assert!(matches!(
        finance_capsules(orphan_flag),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("no matching payment")
    ));
    let mut unflagged = finance_fixture();
    unflagged.retain(|record| record.key != b"link::VmBilling::vm-1");
    assert!(matches!(
        finance_capsules(unflagged),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("no billing-sweep flag")
    ));
}

fn vm_intent_fixture() -> Vec<LegacyPhysicalRecord> {
    let mut records = metadata_fixture_subjects();
    for (column, value) in [
        ("|", vec![1]),
        ("programId", b"program-one".to_vec()),
        ("entityId", b"main".to_vec()),
        ("entityType", b"wasm".to_vec()),
        ("imageName", b"main".to_vec()),
    ] {
        records.push(raw_bytes(
            &format!("obj::Entity::program-one::main::{column}"),
            value,
        ));
    }
    for (key, value) in [
        (
            "link::vmHttpRoute::human-one::api/v1",
            r#"{"programId":"program-one","entityId":"main","vmId":"vm-9","runtime":"wasm"}"#,
        ),
        (
            "link::vmHttpRouteFor::program-one::main",
            "human-one::api/v1",
        ),
        ("link::vmHttpRouteUser::alice", "human-one"),
        ("link::vmAlarmStoreId::program-one", "store-one"),
        ("link::vmAlarmTime::program-one", "1700000000123"),
        ("link::vmAlarmData::program-one", "{\"wake\":true}"),
    ] {
        records.push(raw(key, value));
    }
    records
}

#[test]
fn gateway_routes_and_alarms_migrate_as_durable_intent() {
    let capsules = transform_metadata_fixture(vm_intent_fixture()).unwrap();
    let route = capsules
        .iter()
        .find(|capsule| capsule.kind.0 == "core.gateway_route")
        .unwrap();
    assert!(route.verify().is_ok());
    let Some(CapsuleValue::Object(body)) = &route.body else {
        panic!("route has no body");
    };
    assert_eq!(body["path"], CapsuleValue::Text("api/v1".to_owned()));
    assert_eq!(body["runtime"], CapsuleValue::Text("wasm".to_owned()));
    assert!(!body.contains_key("vm_id"));
    assert_eq!(route.relationships.len(), 2);

    let alarm = capsules
        .iter()
        .find(|capsule| capsule.kind.0 == "core.program_alarm")
        .unwrap();
    let Some(CapsuleValue::Object(body)) = &alarm.body else {
        panic!("alarm has no body");
    };
    assert_eq!(
        body["fire_at_micros"],
        CapsuleValue::Integer(1_700_000_000_123_000)
    );
    // Legacy replays an alarm without an entity as `main`.
    assert_eq!(body["entity_name"], CapsuleValue::Text("main".to_owned()));
    assert_eq!(
        body["data"],
        CapsuleValue::Text("{\"wake\":true}".to_owned())
    );
}

#[test]
fn gateway_route_and_alarm_divergence_fails_closed() {
    let cases: [(&str, Option<&str>, &str); 7] = [
        (
            "link::vmHttpRouteFor::program-one::main",
            Some("human-one::other"),
            "reverse link",
        ),
        (
            "link::vmHttpRouteFor::program-one::main",
            None,
            "reverse link",
        ),
        (
            "link::vmHttpRouteUser::alice",
            Some("program-one"),
            "omits Creature",
        ),
        (
            "link::vmHttpRouteUser::bob",
            Some("human-one"),
            "alias differs",
        ),
        (
            "link::vmHttpRoute::human-one::api/v1",
            Some(r#"{"programId":"program-one","entityId":"main","runtime":"wasm","extra":1}"#),
            "unreviewed",
        ),
        ("link::vmAlarmTime::program-one", Some("later"), "fire time"),
        (
            "link::vmAlarmStoreId::human-one",
            Some("store-one"),
            "never replays",
        ),
    ];
    for (key, value, expected) in cases {
        let mut records = vm_intent_fixture();
        records.retain(|record| record.key != key.as_bytes());
        if let Some(value) = value {
            records.push(raw(key, value));
        }
        match transform_metadata_fixture(records) {
            Err(LegacyMigrationError::Invalid(message)) => {
                assert!(message.contains(expected), "{key}: {message}");
            }
            other => panic!("{key} should fail closed, got {other:?}"),
        }
    }
}

fn vm_resource_fixture() -> Vec<LegacyPhysicalRecord> {
    let mut records = vm_intent_fixture();
    legacy_put_json(
        &mut records,
        "Json::VmResourceStore::vs-1",
        "core",
        &serde_json::json!({"id": "vs-1", "name": "notes", "machineId": "program-one"}),
    );
    legacy_put_json(
        &mut records,
        "Json::VmResourceStore::vs-1",
        "metadata",
        &serde_json::json!({"color": "blue"}),
    );
    records.push(raw("link::vmOwnedStore::program-one::vs-1", "true"));
    for (id, path) in [
        ("e-1", "/data/vm_stores/vs-1/doc/e-1.json"),
        ("e-2", "/data/vm_stores/vs-1/doc/e-2.json"),
    ] {
        let key = format!("Json::VmResourceEntity::vs-1::doc::{id}");
        legacy_put_json(
            &mut records,
            &key,
            "payload",
            &serde_json::json!({"title": id}),
        );
        legacy_put_json(
            &mut records,
            &key,
            "meta",
            &serde_json::json!({"id": id, "storeId": "vs-1", "entityType": "doc", "path": path}),
        );
    }
    records.push(raw(
        "link::vmEntityPath::program-one::main",
        "/data/builds/p1/main.wasm",
    ));
    records.push(raw("link::vmEntityType::program-one::main", "wasm"));
    legacy_put_json(
        &mut records,
        "Json::ProxyEntity::program-one::main",
        "config",
        &serde_json::json!({"upstream": "https://example.test"}),
    );
    records
}

fn vm_resource_evidence(absent_e2: bool) -> LegacyTransformEvidence {
    let present = |store_key: &str, bytes: &[u8]| {
        LegacyPathArtifact::Present(
            LegacyFileArtifactEvidence::from_bytes(store_key, "application/json", bytes).unwrap(),
        )
    };
    let mut path_artifacts = BTreeMap::from([
        (
            "/data/vm_stores/vs-1/doc/e-1.json".to_owned(),
            present("artifacts/e-1", b"{}"),
        ),
        (
            "/data/builds/p1/main.wasm".to_owned(),
            present("artifacts/main.wasm", b"\0asm"),
        ),
    ]);
    if absent_e2 {
        path_artifacts.insert(
            "/data/vm_stores/vs-1/doc/e-2.json".to_owned(),
            LegacyPathArtifact::AttestedAbsent,
        );
    }
    LegacyTransformEvidence {
        path_artifacts,
        ..metadata_evidence()
    }
}

fn vm_resource_capsules(
    records: Vec<LegacyPhysicalRecord>,
    evidence: &LegacyTransformEvidence,
) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
    LegacySnapshotGraph::assemble(records)?.transform_reviewed_with_evidence(140, evidence)
}

#[test]
fn vm_resources_configs_and_artifacts_migrate_with_evidence() {
    let capsules =
        vm_resource_capsules(vm_resource_fixture(), &vm_resource_evidence(true)).unwrap();
    let of = |kind: &str| {
        capsules
            .iter()
            .filter(|capsule| capsule.kind.0 == kind)
            .collect::<Vec<_>>()
    };
    let owner = OwnerScope::Creature(deterministic_legacy_capsule_id("Creature", b"human-one"));
    let stores = of("core.vm_resource_store");
    assert_eq!(stores.len(), 1);
    // A program-owned store belongs to the program's machine creature.
    assert_eq!(stores[0].owner_scope, owner);
    let entities = of("core.vm_resource_entity");
    assert_eq!(entities.len(), 2);
    let present = entities
        .iter()
        .filter(|capsule| matches!(&capsule.body, Some(CapsuleValue::Object(body)) if body["artifact_present"] == CapsuleValue::Bool(true)))
        .count();
    assert_eq!(present, 1);
    assert_eq!(of("core.entity_config").len(), 1);
    let artifacts = of("core.entity_artifact");
    assert_eq!(artifacts.len(), 1);
    assert!(matches!(
        &artifacts[0].body,
        Some(CapsuleValue::Object(body))
            if body["artifact_role"] == CapsuleValue::Text("primary".to_owned())
                && body["store_key"] == CapsuleValue::Text("artifacts/main.wasm".to_owned())
    ));
    assert!(capsules.iter().all(|capsule| capsule.verify().is_ok()));
}

#[test]
fn vm_resource_divergence_and_missing_evidence_fail_closed() {
    // A path with neither copy evidence nor attested absence is never guessed.
    assert!(matches!(
        vm_resource_capsules(vm_resource_fixture(), &vm_resource_evidence(false)),
        Err(LegacyMigrationError::Unmapped { family, .. }) if family == "artifact.evidence"
    ));
    let evidence = vm_resource_evidence(true);
    let mut unowned = vm_resource_fixture();
    unowned.retain(|record| record.key != b"link::vmOwnedStore::program-one::vs-1");
    assert!(vm_resource_capsules(unowned, &evidence).is_err());

    let mut wrong_type = vm_resource_fixture();
    set_link(
        &mut wrong_type,
        "link::vmEntityType::program-one::main",
        "docker",
    );
    assert!(matches!(
        vm_resource_capsules(wrong_type, &evidence),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("disagrees with the entity")
    ));

    let mut typeless = vm_resource_fixture();
    typeless.retain(|record| record.key != b"link::vmEntityType::program-one::main");
    assert!(vm_resource_capsules(typeless, &evidence).is_err());

    let mut orphan_entity = vm_resource_fixture();
    legacy_put_json(
        &mut orphan_entity,
        "Json::VmResourceEntity::vs-9::doc::e-9",
        "meta",
        &serde_json::json!({"id": "e-9", "storeId": "vs-9", "entityType": "doc", "path": "/x"}),
    );
    assert!(matches!(
        vm_resource_capsules(orphan_entity, &evidence),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("names no resource store")
    ));
}

fn legacy_encrypt(key: &[u8; 32], nonce: [u8; 12], plaintext: &[u8]) -> String {
    use base64::Engine as _;
    use chacha20poly1305::aead::{Aead, KeyInit};
    let ciphertext =
        chacha20poly1305::ChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(key))
            .encrypt(chacha20poly1305::Nonce::from_slice(&nonce), plaintext)
            .unwrap();
    let mut blob = nonce.to_vec();
    blob.extend(ciphertext);
    base64::engine::general_purpose::STANDARD.encode(blob)
}

fn secret_fixture(key: &[u8; 32]) -> Vec<LegacyPhysicalRecord> {
    let mut records = metadata_fixture_subjects();
    records.push(raw(
        "link::Secret::human-one::api-token",
        &legacy_encrypt(key, [3; 12], b"s3cr3t-value"),
    ));
    records.push(raw(
        "link::SecretGrant::human-one::api-token::program-one",
        "1700000000000",
    ));
    records.push(raw(
        "link::SecretGrantee::program-one::human-one::api-token",
        "1700000000000",
    ));
    records.push(raw(
        "link::LoginGrant::abc123",
        "1700000000000|alice@example.test",
    ));
    records
}

fn secret_capsules(
    records: Vec<LegacyPhysicalRecord>,
    key: Option<[u8; 32]>,
) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
    use base64::Engine as _;
    let evidence = LegacyTransformEvidence {
        secret_master_key: key.map(|key| {
            LegacySecretMasterKey::from_key_file(
                &base64::engine::general_purpose::STANDARD.encode(key),
            )
            .unwrap()
        }),
        ..metadata_evidence()
    };
    LegacySnapshotGraph::assemble(records)?.transform_reviewed_with_evidence(140, &evidence)
}

#[test]
fn secrets_migrate_as_authenticated_ciphertext_with_grants() {
    let key = [9_u8; 32];
    let capsules = secret_capsules(secret_fixture(&key), Some(key)).unwrap();
    let secret = capsules
        .iter()
        .find(|capsule| capsule.kind.0 == "core.creature_secret")
        .unwrap();
    let Some(CapsuleValue::Object(body)) = &secret.body else {
        panic!("secret has no body");
    };
    assert_eq!(
        body["algorithm"],
        CapsuleValue::Text(LEGACY_SECRET_ALGORITHM.to_owned())
    );
    let master = {
        use base64::Engine as _;
        LegacySecretMasterKey::from_key_file(&base64::engine::general_purpose::STANDARD.encode(key))
            .unwrap()
    };
    assert_eq!(
        body["key_fingerprint"],
        CapsuleValue::Bytes(master.fingerprint().to_vec())
    );
    assert!(!format!("{master:?}").contains('9'));
    // Plaintext never appears in any exported capsule.
    for capsule in &capsules {
        let encoded = capsule.canonical_bytes().unwrap();
        assert!(!encoded.windows(12).any(|window| window == b"s3cr3t-value"));
    }
    let grant = capsules
        .iter()
        .find(|capsule| capsule.kind.0 == "core.secret_grant")
        .unwrap();
    assert!(matches!(
        &grant.body,
        Some(CapsuleValue::Object(body)) if body["expires_at_micros"] == CapsuleValue::Integer(1_700_000_000_000_000)
    ));
    // The grantee is a program, not a creature, so no creature relationship is invented.
    assert_eq!(grant.relationships.len(), 1);
    assert!(capsules.iter().all(|capsule| capsule.verify().is_ok()));
}

#[test]
fn secret_authentication_and_grant_mirrors_fail_closed() {
    let key = [9_u8; 32];
    assert!(matches!(
        secret_capsules(secret_fixture(&key), None),
        Err(LegacyMigrationError::Unmapped { family, .. }) if family == "Secret.master_key"
    ));
    assert!(matches!(
        secret_capsules(secret_fixture(&key), Some([8; 32])),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("does not authenticate")
    ));
    let mut tampered = secret_fixture(&key);
    set_link(
        &mut tampered,
        "link::Secret::human-one::api-token",
        &legacy_encrypt(&[1; 32], [3; 12], b"x"),
    );
    assert!(secret_capsules(tampered, Some(key)).is_err());

    let mut unmirrored = secret_fixture(&key);
    unmirrored
        .retain(|record| record.key != b"link::SecretGrantee::program-one::human-one::api-token");
    assert!(matches!(
        secret_capsules(unmirrored, Some(key)),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("mirror")
    ));

    let mut dangling = secret_fixture(&key);
    dangling.push(raw("link::SecretGrant::human-one::gone::program-one", "5"));
    dangling.push(raw(
        "link::SecretGrantee::program-one::human-one::gone",
        "5",
    ));
    assert!(matches!(
        secret_capsules(dangling, Some(key)),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("names no secret")
    ));

    let mut bad_login = secret_fixture(&key);
    set_link(&mut bad_login, "link::LoginGrant::abc123", "whenever");
    assert!(secret_capsules(bad_login, Some(key)).is_err());
}

#[test]
fn bridge_grants_migrate_by_digest_with_topic_claims() {
    let digest = "ab".repeat(32);
    let mut records = metadata_fixture_subjects();
    legacy_put_json(
        &mut records,
        &format!("Json::BridgeGrant::{digest}"),
        "grant",
        &serde_json::json!({
            "creatureId": "program-one", "deliverTo": "human-one", "routes": {},
            "topics": ["updates"], "expiresAt": 0, "createdAt": 5
        }),
    );
    records.push(raw("link::BridgeTopicOwner::updates", "program-one"));
    records.push(raw("link::BridgeTopicOwner::orphaned", "program-one"));
    let capsules = transform_metadata_fixture(records.clone()).unwrap();
    let grant = capsules
        .iter()
        .find(|capsule| capsule.kind.0 == "core.bridge_grant")
        .unwrap();
    assert!(matches!(
        &grant.body,
        Some(CapsuleValue::Object(body))
            if body["token_digest"] == CapsuleValue::Bytes(vec![0xab; 32])
                && body["expires_at_micros"] == CapsuleValue::Integer(0)
    ));
    // A program-minted grant belongs to the program's machine creature.
    assert_eq!(
        grant.owner_scope,
        OwnerScope::Creature(deterministic_legacy_capsule_id("Creature", b"human-one"))
    );
    assert_eq!(
        capsules
            .iter()
            .filter(|capsule| capsule.kind.0 == "core.bridge_topic")
            .count(),
        2
    );

    let mut unclaimed = records.clone();
    unclaimed.retain(|record| record.key != b"link::BridgeTopicOwner::updates");
    assert!(matches!(
        transform_metadata_fixture(unclaimed),
        Err(LegacyMigrationError::Invalid(message)) if message.contains("no BridgeTopicOwner claim")
    ));
    let mut bad_digest = metadata_fixture_subjects();
    legacy_put_json(
        &mut bad_digest,
        "Json::BridgeGrant::not-a-digest",
        "grant",
        &serde_json::json!({"creatureId": "program-one"}),
    );
    assert!(transform_metadata_fixture(bad_digest).is_err());
    let mut address = metadata_fixture_subjects();
    address.push(raw("link::NodeIpToHost::10.0.0.1", "node-a"));
    assert!(matches!(
        transform_metadata_fixture(address),
        Err(LegacyMigrationError::Unmapped { family, .. }) if family == "link.NodeIpToHost"
    ));
}

#[test]
fn hashgraph_store_is_classified_strictly_and_checkpointed_read_only() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "aseman-hashgraph-test-{}-{unique}",
        std::process::id()
    ));
    {
        let database = DB::open_default(&path).unwrap();
        for (key, value) in [
            ("rep_ab12", "peer"),
            ("peerset_000000001", "set"),
            ("topo_000000001", "event"),
            ("ab12__event_000000001", "event"),
            ("ab12_root", "root"),
            ("round_000000001", "round"),
            ("block_000000001", "b1"),
            ("block_000000002", "b2"),
            ("frame_000000001", "frame"),
        ] {
            database.put(key, value).unwrap();
        }
    }
    let checkpoint = LegacyHashgraphCheckpoint::read_only(&path).unwrap();
    assert_eq!(checkpoint.family_counts.len(), 8);
    assert_eq!(checkpoint.highest_block, Some(2));
    // Deterministic across reads and sensitive to finalized history.
    assert_eq!(
        LegacyHashgraphCheckpoint::read_only(&path).unwrap(),
        checkpoint
    );
    let changed = LegacyHashgraphCheckpoint::from_records([
        (&b"block_000000001"[..], &b"b1"[..]),
        (&b"block_000000002"[..], &b"forked"[..]),
    ])
    .unwrap();
    assert_ne!(changed.block_digest, checkpoint.block_digest);
    DB::destroy(&Options::default(), &path).unwrap();

    for unknown in ["block_12", "mystery", "__event_000000001"] {
        assert!(matches!(
            LegacyHashgraphCheckpoint::from_records([(unknown.as_bytes(), &b"x"[..])]),
            Err(LegacyMigrationError::Unmapped { family, .. }) if family == "hashgraph"
        ));
    }
}

#[test]
fn legacy_kv_store_scans_exact_prefixes_and_writes_atomically() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("aseman-kv-test-{}-{unique}", std::process::id()));
    {
        let store = RocksDbKvStore::open_default(&path).unwrap();
        store
            .write_batch(&[
                LegacyKvWrite::Put {
                    key: b"link::a::1".to_vec(),
                    value: b"x".to_vec(),
                },
                LegacyKvWrite::Put {
                    key: b"link::a::2".to_vec(),
                    value: b"y".to_vec(),
                },
                LegacyKvWrite::Put {
                    key: b"link::b::1".to_vec(),
                    value: b"z".to_vec(),
                },
                LegacyKvWrite::Put {
                    key: b"obj::A".to_vec(),
                    value: b"o".to_vec(),
                },
            ])
            .unwrap();
        let scanned = store.scan_prefix(b"link::a::").unwrap();
        assert_eq!(
            scanned
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>(),
            vec![b"link::a::1".to_vec(), b"link::a::2".to_vec()]
        );
        store
            .write_batch(&[
                LegacyKvWrite::Delete {
                    key: b"link::a::1".to_vec(),
                },
                LegacyKvWrite::Put {
                    key: b"link::a::3".to_vec(),
                    value: b"w".to_vec(),
                },
            ])
            .unwrap();
        assert_eq!(store.get(b"link::a::1").unwrap(), None);
        assert_eq!(store.get(b"link::a::3").unwrap(), Some(b"w".to_vec()));
        assert_eq!(store.scan_all().unwrap().len(), 4);
        assert!(store.scan_prefix(b"zzz").unwrap().is_empty());
    }
    DB::destroy(&Options::default(), &path).unwrap();
}

#[test]
fn legacy_identity_map_covers_entities_but_never_sessions() {
    let mut records = metadata_fixture_subjects();
    for (column, value) in [("|", vec![1]), ("userId", b"human-one".to_vec())] {
        records.push(raw_bytes(
            &format!("obj::Session::secret-token::{column}"),
            value,
        ));
    }
    records.push(raw("index::Session::userId::id::human-one", "secret-token"));
    let capsules = transform_metadata_fixture(records).unwrap();
    let identities = capsules
        .iter()
        .filter(|capsule| capsule.kind.0 == "core.legacy_identity")
        .filter_map(|capsule| match &capsule.body {
            Some(CapsuleValue::Object(body)) => {
                Some((body["family"].clone(), body["legacy_id"].clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let text = |value: &str| CapsuleValue::Text(value.to_owned());
    for expected in [
        ("Creature", "human-one"),
        ("User", "human-one"),
        ("Program", "program-one"),
        ("Store", "store-one"),
    ] {
        assert!(
            identities.contains(&(text(expected.0), text(expected.1))),
            "missing {expected:?}"
        );
    }
    assert!(
        !identities
            .iter()
            .any(|(_, legacy)| legacy == &text("secret-token"))
    );
    for capsule in &capsules {
        let encoded = capsule.canonical_bytes().unwrap();
        assert!(!encoded.windows(12).any(|window| window == b"secret-token"));
    }
}

/// LD-12: the audit finds exactly the membership links the ADR 0018 export refuses.
/// The repair removes only the unambiguous ones, and only under the approved digest.
#[test]
fn membership_audit_repairs_ld12_residue_only_under_the_approved_digest() {
    let origins = ["global", "local.example"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let mut records = membership_fixture();
    for (key, value) in [
        // A deleted local creature kept its membership (LD-12).
        ("link::onaccess::store-one::9@local.example", "read"),
        ("link::hasaccess::9@local.example::store-one", "true"),
        // A membership in a store whose object is gone.
        ("link::onaccess::gone-store::human-one", "read"),
        ("link::hasaccess::human-one::gone-store", "true"),
        // LD-11: a grant without the member flag.
        ("link::onaccess::store-one::8@remote.example", "read"),
        // A store whose creator was deleted.
        ("link::creatorof::9@local.example::store-two", "true"),
        ("obj::Store::store-two::|", "\u{1}"),
    ] {
        records.push(raw(key, value));
    }
    assert!(membership_capsules(records.clone(), &["global", "local.example"]).is_err());

    let audit = LegacySnapshotGraph::assemble(records.clone())
        .unwrap()
        .membership_audit(&origins)
        .unwrap();
    let summary = audit
        .findings
        .iter()
        .map(|finding| {
            (
                finding.defect,
                finding.store.as_str(),
                finding.principal.as_str(),
                finding.removals.len(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            (
                LegacyMembershipDefect::MissingStore,
                "gone-store",
                "human-one",
                2
            ),
            (
                LegacyMembershipDefect::DanglingLocalMember,
                "store-one",
                "9@local.example",
                2
            ),
            (
                LegacyMembershipDefect::OneSidedPair,
                "store-one",
                "8@remote.example",
                0
            ),
            (
                LegacyMembershipDefect::OrphanedCreator,
                "store-two",
                "9@local.example",
                0
            ),
        ]
    );

    let path = std::env::temp_dir().join(format!(
        "aseman-membership-repair-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = RocksDbKvStore::open_default(&path).unwrap();
    store
        .write_batch(
            &records
                .iter()
                .map(|record| LegacyKvWrite::Put {
                    key: record.key.clone(),
                    value: record.value.clone(),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert_eq!(audit_legacy_memberships(&store, &origins).unwrap(), audit);

    // A stale approval writes nothing.
    let mut stale = audit.digest;
    stale[0] ^= 1;
    assert!(repair_legacy_memberships(&store, &origins, stale).is_err());
    assert_eq!(audit_legacy_memberships(&store, &origins).unwrap(), audit);

    let report = repair_legacy_memberships(&store, &origins, audit.digest).unwrap();
    assert_eq!(
        report.removed_keys,
        [
            "link::hasaccess::9@local.example::store-one",
            "link::hasaccess::human-one::gone-store",
            "link::onaccess::gone-store::human-one",
            "link::onaccess::store-one::9@local.example",
        ]
    );
    assert_eq!(report.needs_decision.len(), 2);
    let remaining = audit_legacy_memberships(&store, &origins).unwrap();
    assert_eq!(remaining.findings, report.needs_decision);

    // Once an operator resolves the two decisions, the export accepts every link.
    store
        .write_batch(&[
            LegacyKvWrite::Delete {
                key: b"link::onaccess::store-one::8@remote.example".to_vec(),
            },
            LegacyKvWrite::Delete {
                key: b"link::creatorof::9@local.example::store-two".to_vec(),
            },
            LegacyKvWrite::Delete {
                key: b"obj::Store::store-two::|".to_vec(),
            },
        ])
        .unwrap();
    assert!(
        audit_legacy_memberships(&store, &origins)
            .unwrap()
            .is_clean()
    );
    let repaired = store
        .scan_all()
        .unwrap()
        .into_iter()
        .map(|(key, value)| LegacyPhysicalRecord {
            family: "application-rocksdb-default".to_owned(),
            key,
            value,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        membership_capsules(repaired, &["global", "local.example"])
            .unwrap()
            .len(),
        3
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

/// LD-16: derived `ownerof` links are rebuilt from each creature's `ownerId`.
#[test]
fn owner_link_repair_rebuilds_links_from_owner_ids() {
    let origins = ["global", "local.example"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let mut records = membership_fixture();
    for (key, value) in [
        // A machine whose link names a previous owner.
        ("obj::Creature::bot-one::|", "\u{1}"),
        ("obj::Creature::bot-one::type", "machine"),
        ("obj::Creature::bot-one::ownerId", "human-one"),
        ("link::ownerof::someone-else::bot-one", "true"),
        // A machine with no owner at all: nothing can be derived.
        ("obj::Creature::bot-two::|", "\u{1}"),
        ("obj::Creature::bot-two::type", "machine"),
        ("obj::Creature::bot-two::ownerId", ""),
        // A link left behind by a deleted machine.
        ("link::ownerof::human-one::deleted-bot", "true"),
    ] {
        records.push(raw(key, value));
    }
    let audit = LegacySnapshotGraph::assemble(records.clone())
        .unwrap()
        .membership_audit(&origins)
        .unwrap();
    let owner_findings = audit
        .findings
        .iter()
        .filter(|finding| finding.store.is_empty())
        .map(|finding| (finding.defect, finding.principal.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        owner_findings,
        [
            (LegacyMembershipDefect::DanglingOwnerLink, "deleted-bot"),
            (LegacyMembershipDefect::StaleOwnerLink, "bot-one"),
            (LegacyMembershipDefect::MissingOwnerLink, "bot-one"),
            (LegacyMembershipDefect::MissingOwnerLink, "bot-two"),
        ]
    );

    let path = std::env::temp_dir().join(format!(
        "aseman-owner-repair-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = RocksDbKvStore::open_default(&path).unwrap();
    store
        .write_batch(
            &records
                .iter()
                .map(|record| LegacyKvWrite::Put {
                    key: record.key.clone(),
                    value: record.value.clone(),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let report = repair_legacy_memberships(&store, &origins, audit.digest).unwrap();
    assert_eq!(
        report.removed_keys,
        [
            "link::ownerof::human-one::deleted-bot",
            "link::ownerof::someone-else::bot-one",
        ]
    );
    assert_eq!(report.added_keys, ["link::ownerof::human-one::bot-one"]);
    let remaining = audit_legacy_memberships(&store, &origins).unwrap();
    assert_eq!(remaining.findings, report.needs_decision);
    assert_eq!(remaining.findings.len(), 1);
    assert_eq!(remaining.findings[0].principal, "bot-two");
    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}
