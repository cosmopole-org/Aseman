//! Infra-free unit tests for the administration drivers: hashing, Ed25519 signing
//! and verification, and manifest signing determinism. The journal state machine
//! itself is covered in `aseman-domain::operations`.

use std::collections::BTreeMap;
use std::fs;

use super::*;

fn sample_manifest() -> BackupManifest {
    let mut capsule = BTreeMap::new();
    capsule.insert("core".to_owned(), 1);
    let mut mappings = BTreeMap::new();
    mappings.insert("core_storage".to_owned(), "legacy".to_owned());
    let mut versions = BTreeMap::new();
    versions.insert("aseman-node".to_owned(), "test".to_owned());
    BackupManifest {
        version: 1,
        backup_id: uuid::Uuid::now_v7().to_string(),
        created_at: "2026-09-25T00:00:00Z".to_owned(),
        source_cluster_id: "node1".to_owned(),
        capsule_schema_versions: capsule,
        provider_mappings: mappings,
        module_versions: versions,
        artifacts: vec![Artifact {
            logical_name: "snapshot/root/data.bin".to_owned(),
            media_type: "application/octet-stream".to_owned(),
            size_bytes: 3,
            sha256: "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
        }],
        signature: None,
    }
}

#[test]
fn hash_file_is_deterministic_and_sha256() {
    let dir = std::env::temp_dir().join(format!("asemanctl-hash-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.bin");
    fs::write(&path, b"hello aseman").unwrap();

    let first = hash_file(&path).unwrap();
    let second = hash_file(&path).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.len(), 64);
    // Both helpers must agree with the SHA-256 of the exact bytes.
    assert_eq!(first, hash_bytes(b"hello aseman"));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn hex_round_trips() {
    let bytes = vec![0u8, 1, 0xab, 0xff, 0x80];
    let encoded = encode_hex(&bytes);
    assert_eq!(decode_hex(&encoded).unwrap(), bytes);
}

#[test]
fn ed25519_signs_and_verifies() {
    let seed = [7u8; 32];
    let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&seed).expect("valid seed");
    let message = b"backup-manifest v1 node1";
    let (key_id, signature) = sign_bytes(&key, message);
    assert_eq!(key_id.len(), 64);
    verify_bytes(&key_id, &signature, message).expect("valid signature verifies");
}

#[test]
fn ed25519_signature_rejects_tampering() {
    let seed = [9u8; 32];
    let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&seed).expect("valid seed");
    let (key_id, signature) = sign_bytes(&key, b"original");
    assert!(verify_bytes(&key_id, &signature, b"tampered").is_err());
}

#[test]
fn manifest_signing_bytes_are_stable_before_and_after_signing() {
    let mut manifest = sample_manifest();
    let unsigned = manifest.signing_bytes().unwrap();
    manifest.signature = Some(Signature {
        algorithm: "ed25519".to_owned(),
        key_id: "a".repeat(64),
        value: "b".repeat(128),
    });
    // The pre-signature payload is what gets signed, so stripping the signature for
    // verification reproduces the exact bytes.
    assert_eq!(manifest.signing_bytes().unwrap(), unsigned);
    // The written form still carries the signature.
    let serialized = serde_json::to_string(&manifest).unwrap();
    assert!(serialized.contains("\"signature\""));
}

#[test]
fn signing_and_verification_round_trip_a_manifest() {
    let seed = [11u8; 32];
    let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&seed).expect("valid seed");
    let mut manifest = sample_manifest();
    let (key_id, value) = sign_bytes(&key, &manifest.signing_bytes().unwrap());
    manifest.signature = Some(Signature {
        algorithm: "ed25519".to_owned(),
        key_id,
        value,
    });
    let written = serde_json::to_vec_pretty(&manifest).unwrap();
    let parsed: BackupManifest = serde_json::from_slice(&written).unwrap();
    let signature = parsed
        .signature
        .as_ref()
        .expect("manifest carries a signature");
    verify_bytes(
        &signature.key_id,
        &signature.value,
        &parsed.signing_bytes().unwrap(),
    )
    .expect("the written manifest verifies after a serde round trip");
}

#[test]
fn step_labels_cover_every_operation_step() {
    for kind in [
        OperationKind::Upgrade,
        OperationKind::Backup,
        OperationKind::Restore,
        OperationKind::Doctor,
        OperationKind::SupportBundle,
    ] {
        for step in kind.steps() {
            assert!(!step_label(*step).is_empty());
        }
    }
}

#[test]
fn copy_tree_reproduces_a_nested_directory() {
    let base = std::env::temp_dir().join(format!("asemanctl-tree-{}", std::process::id()));
    let source = base.join("src");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::write(source.join("nested/a.txt"), "a").unwrap();
    fs::write(source.join("b.txt"), "bb").unwrap();

    let target = base.join("dst");
    copy_tree(&source, &target).unwrap();
    assert_eq!(
        fs::read_to_string(target.join("nested/a.txt")).unwrap(),
        "a"
    );
    assert_eq!(fs::read_to_string(target.join("b.txt")).unwrap(), "bb");
    assert_eq!(walk_files(&target).unwrap().len(), 2);
    fs::remove_dir_all(&base).ok();
}
