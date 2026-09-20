use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleRelationship, CapsuleValue,
    DIGEST_ALGORITHM, ENCODING_VERSION, OwnerScope, StorageClass,
};
use std::collections::BTreeMap;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn main() {
    let capsule = CapsuleEnvelope {
        encoding_version: ENCODING_VERSION,
        id: CapsuleId([1; 16]),
        kind: CapsuleKind("core.program".to_owned()),
        storage_class: StorageClass::Core,
        owner_scope: OwnerScope::Creature([2; 16]),
        schema_version: 1,
        revision: 1,
        created_at_micros: 1_700_000_000_000_000,
        updated_at_micros: 1_700_000_000_000_000,
        previous_integrity: None,
        integrity_hash: CapsuleDigest {
            algorithm: DIGEST_ALGORITHM.to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: false,
        relationships: vec![CapsuleRelationship {
            name: "creature".to_owned(),
            target_kind: CapsuleKind("core.creature".to_owned()),
            target_id: CapsuleId([2; 16]),
        }],
        body: Some(CapsuleValue::Object(BTreeMap::from([
            ("name".to_owned(), CapsuleValue::Text("demo".to_owned())),
            ("weight".to_owned(), CapsuleValue::Float(1.5)),
        ]))),
    }
    .seal()
    .expect("vector capsule seals");
    println!("integrity={}", hex(&capsule.integrity_hash.bytes));
    println!(
        "canonical={}",
        hex(&capsule.canonical_bytes().expect("vector encodes"))
    );
}
