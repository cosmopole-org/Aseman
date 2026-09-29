//! Shared legacy bridge contracts: deterministic legacy identities and the store-signal
//! realtime event encoding used by both the migration export and live adapters, so
//! migrated history and new appends are indistinguishable.

use crate::capsule::{
    CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleResult, CapsuleValue,
    OwnerScope, StorageClass, encode_value,
};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

pub const STORE_SIGNAL_EVENT_TYPE: &str = "legacy.store.signal";

/// Domain-separated deterministic capsule ID for a legacy identity (first 128 bits).
#[must_use]
pub fn deterministic_legacy_capsule_id(family: &str, legacy_id: &[u8]) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(b"ASEMAN-LEGACY-CAPSULE-ID-V1\0");
    hasher.update((family.len() as u64).to_be_bytes());
    hasher.update(family.as_bytes());
    hasher.update((legacy_id.len() as u64).to_be_bytes());
    hasher.update(legacy_id);
    hasher.finalize()[..16]
        .try_into()
        .expect("fixed digest slice")
}

/// Stream authorization scope and retention, resolved server-side per store (P7-04
/// owns the rule; the migration runner and live composition supply the same policy).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignalStreamPolicy {
    pub authorization_scope: Vec<u8>,
    pub retention_class: String,
}

impl SignalStreamPolicy {
    /// The default policy for a store's signal stream until P7-04 refines the rule:
    /// the stream is authorized by its store, and its retention is persistent (only
    /// stores with persistent history record signals). Live composition and the
    /// migration runner use the same rule, so migrated and new events agree.
    #[must_use]
    pub fn for_store(store_id: &str) -> Self {
        Self {
            authorization_scope: deterministic_legacy_capsule_id("Store", store_id.as_bytes())
                .to_vec(),
            retention_class: "persistent".to_owned(),
        }
    }
}

/// One store signal as carried in a realtime event payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreSignalPayload {
    pub signal_id: String,
    pub store_id: String,
    pub sender_id: String,
    pub data: String,
    pub tags: Vec<String>,
    pub edited: bool,
}

#[must_use]
pub fn store_signal_stream(store_id: &str) -> String {
    format!("store:{store_id}")
}

fn hex_digest(domain: &[u8], value: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Build the sealed `realtime.event` capsule for one store signal.
pub fn store_signal_event(
    signal: &StoreSignalPayload,
    sequence: i64,
    occurred_at_micros: i64,
    policy: &SignalStreamPolicy,
) -> CapsuleResult<CapsuleEnvelope> {
    let payload = encode_value(&CapsuleValue::Object(BTreeMap::from([
        (
            "legacy_id".to_owned(),
            CapsuleValue::Text(signal.signal_id.clone()),
        ),
        (
            "store_id".to_owned(),
            CapsuleValue::Text(signal.store_id.clone()),
        ),
        (
            "user_id".to_owned(),
            CapsuleValue::Text(signal.sender_id.clone()),
        ),
        ("data".to_owned(), CapsuleValue::Text(signal.data.clone())),
        (
            "tags".to_owned(),
            CapsuleValue::Array(
                signal
                    .tags
                    .iter()
                    .cloned()
                    .map(CapsuleValue::Text)
                    .collect(),
            ),
        ),
        ("edited".to_owned(), CapsuleValue::Bool(signal.edited)),
    ])))?;
    let payload_digest = Sha256::digest(&payload).to_vec();
    CapsuleEnvelope {
        encoding_version: 1,
        id: CapsuleId(deterministic_legacy_capsule_id(
            "QuestDB.storage",
            signal.signal_id.as_bytes(),
        )),
        kind: CapsuleKind("realtime.event".to_owned()),
        storage_class: StorageClass::Realtime,
        owner_scope: OwnerScope::Global,
        schema_version: 1,
        revision: 1,
        created_at_micros: occurred_at_micros,
        updated_at_micros: occurred_at_micros,
        previous_integrity: None,
        integrity_hash: CapsuleDigest {
            algorithm: "sha2-256".to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: false,
        relationships: Vec::new(),
        body: Some(CapsuleValue::Object(BTreeMap::from([
            (
                "stream_id".to_owned(),
                CapsuleValue::Text(store_signal_stream(&signal.store_id)),
            ),
            ("sequence".to_owned(), CapsuleValue::Integer(sequence)),
            (
                "event_type".to_owned(),
                CapsuleValue::Text(STORE_SIGNAL_EVENT_TYPE.to_owned()),
            ),
            (
                "authorization_scope".to_owned(),
                CapsuleValue::Bytes(policy.authorization_scope.clone()),
            ),
            (
                "producer".to_owned(),
                CapsuleValue::Text(signal.sender_id.clone()),
            ),
            (
                "trace_id".to_owned(),
                CapsuleValue::Text(hex_digest(
                    b"ASEMAN-LEGACY-SIGNAL-TRACE-V1\0",
                    signal.signal_id.as_bytes(),
                )),
            ),
            (
                "correlation_id".to_owned(),
                CapsuleValue::Text(String::new()),
            ),
            ("payload".to_owned(), CapsuleValue::Bytes(payload)),
            (
                "payload_digest".to_owned(),
                CapsuleValue::Bytes(payload_digest),
            ),
            (
                "retention_class".to_owned(),
                CapsuleValue::Text(policy.retention_class.clone()),
            ),
            (
                "idempotency_key".to_owned(),
                CapsuleValue::Text(signal.signal_id.clone()),
            ),
            (
                "occurred_at_micros".to_owned(),
                CapsuleValue::Integer(occurred_at_micros),
            ),
        ]))),
    }
    .seal()
}

/// Decode a store-signal event back into its payload, verifying the payload digest.
#[must_use]
pub fn decode_store_signal(event: &CapsuleEnvelope) -> Option<(StoreSignalPayload, i64)> {
    let Some(CapsuleValue::Object(body)) = &event.body else {
        return None;
    };
    if body.get("event_type") != Some(&CapsuleValue::Text(STORE_SIGNAL_EVENT_TYPE.to_owned())) {
        return None;
    }
    let (
        Some(CapsuleValue::Bytes(payload)),
        Some(CapsuleValue::Bytes(digest)),
        Some(CapsuleValue::Integer(at)),
    ) = (
        body.get("payload"),
        body.get("payload_digest"),
        body.get("occurred_at_micros"),
    )
    else {
        return None;
    };
    if Sha256::digest(payload).as_slice() != digest.as_slice() {
        return None;
    }
    let CapsuleValue::Object(fields) = crate::capsule::decode_canonical_value(payload).ok()? else {
        return None;
    };
    let text = |name: &str| match fields.get(name) {
        Some(CapsuleValue::Text(value)) => Some(value.clone()),
        _ => None,
    };
    let tags = match fields.get("tags") {
        Some(CapsuleValue::Array(items)) => items
            .iter()
            .map(|item| match item {
                CapsuleValue::Text(tag) => Some(tag.clone()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    let edited = matches!(fields.get("edited"), Some(CapsuleValue::Bool(true)));
    Some((
        StoreSignalPayload {
            signal_id: text("legacy_id")?,
            store_id: text("store_id")?,
            sender_id: text("user_id")?,
            data: text("data")?,
            tags,
            edited,
        },
        *at,
    ))
}
