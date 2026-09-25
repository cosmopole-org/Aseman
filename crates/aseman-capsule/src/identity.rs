//! The identity key directory on the capsule protocol (A401 section 9): one
//! `core.identity_key` capsule per key epoch, found by its unique key ID or by
//! (subject, purpose). Keys are never deleted; retirement and revocation are revisions.

use crate::store::{body, next_revision, port_error};
use crate::support::{MAX_CAS_ATTEMPTS, equal, failed, new_capsule, text};
use crate::{CapsuleStore, CapsuleStoreError};
use aseman_contracts::capsule::{
    CapsuleEnvelope, CapsuleKind, CapsuleQuery, CapsuleValue, MAX_QUERY_LIMIT, OwnerScope,
    QueryPredicate, StorageClass,
};
use aseman_domain::identity::{IdentityKey, KeyEpoch, KeyPurpose, Subject, SubjectKind};
use aseman_ports::{KeyDirectory, PortError, PortResult};
use std::collections::{BTreeMap, BTreeSet};

const IDENTITY_KEY: &str = "core.identity_key";

/// The key directory over any [`CapsuleStore`].
pub struct CapsuleKeyDirectory<'a> {
    pub repository: &'a dyn CapsuleStore,
}

const PURPOSES: [KeyPurpose; 4] = [
    KeyPurpose::Authentication,
    KeyPurpose::Descriptor,
    KeyPurpose::TokenIssuing,
    KeyPurpose::Introduction,
];

fn micros(millis: i64) -> PortResult<CapsuleValue> {
    millis
        .checked_mul(1_000)
        .map(CapsuleValue::Integer)
        .ok_or_else(|| failed("key time overflows microseconds"))
}

fn optional_micros(millis: Option<i64>) -> PortResult<Option<CapsuleValue>> {
    millis.map(micros).transpose()
}

fn fields(key: &IdentityKey) -> PortResult<BTreeMap<String, CapsuleValue>> {
    let epoch = &key.epoch;
    let mut fields = BTreeMap::from([
        (
            "subject_kind".to_owned(),
            CapsuleValue::Text(epoch.subject.kind.as_str().to_owned()),
        ),
        (
            "subject_id".to_owned(),
            CapsuleValue::Bytes(epoch.subject.id.as_bytes().to_vec()),
        ),
        (
            "purpose".to_owned(),
            CapsuleValue::Text(epoch.purpose.as_str().to_owned()),
        ),
        (
            "epoch".to_owned(),
            CapsuleValue::Integer(i64::from(epoch.epoch)),
        ),
        ("key_id".to_owned(), CapsuleValue::Text(key.key_id.clone())),
        (
            "public_key".to_owned(),
            CapsuleValue::Bytes(key.public_key.clone()),
        ),
        (
            "not_before_micros".to_owned(),
            micros(epoch.not_before_millis)?,
        ),
        ("legacy".to_owned(), CapsuleValue::Bool(epoch.legacy)),
    ]);
    for (name, value) in [
        ("expires_at_micros", epoch.expires_at_millis),
        ("retired_at_micros", epoch.retired_at_millis),
        ("revoked_at_micros", epoch.revoked_at_millis),
    ] {
        if let Some(value) = optional_micros(value)? {
            fields.insert(name.to_owned(), value);
        }
    }
    Ok(fields)
}

fn millis(fields: &BTreeMap<String, CapsuleValue>, name: &str) -> PortResult<Option<i64>> {
    match fields.get(name) {
        None | Some(CapsuleValue::Null) => Ok(None),
        Some(CapsuleValue::Integer(micros)) => Ok(Some(micros / 1_000)),
        Some(_) => Err(failed(format!("identity key {name} is not a time"))),
    }
}

fn record(capsule: &CapsuleEnvelope) -> PortResult<IdentityKey> {
    let fields = body(capsule).ok_or(PortError::NotFound)?;
    let kind = text(fields, "subject_kind");
    let kind = SubjectKind::ALL
        .into_iter()
        .find(|candidate| candidate.as_str() == kind)
        .ok_or_else(|| failed(format!("unknown subject kind {kind}")))?;
    let id = match fields.get("subject_id") {
        Some(CapsuleValue::Bytes(bytes)) => uuid::Uuid::from_slice(bytes).map_err(failed)?,
        _ => return Err(failed("identity key has no subject")),
    };
    let purpose = text(fields, "purpose");
    let purpose = PURPOSES
        .into_iter()
        .find(|candidate| candidate.as_str() == purpose)
        .ok_or_else(|| failed(format!("unknown key purpose {purpose}")))?;
    let epoch = match fields.get("epoch") {
        Some(CapsuleValue::Integer(epoch)) => u32::try_from(*epoch).map_err(failed)?,
        _ => return Err(failed("identity key has no epoch")),
    };
    let public_key = match fields.get("public_key") {
        Some(CapsuleValue::Bytes(bytes)) => bytes.clone(),
        _ => return Err(failed("identity key has no public key")),
    };
    Ok(IdentityKey {
        key_id: text(fields, "key_id"),
        public_key,
        epoch: KeyEpoch {
            subject: Subject { kind, id },
            purpose,
            epoch,
            not_before_millis: millis(fields, "not_before_micros")?
                .ok_or_else(|| failed("identity key has no validity start"))?,
            expires_at_millis: millis(fields, "expires_at_micros")?,
            retired_at_millis: millis(fields, "retired_at_micros")?,
            revoked_at_millis: millis(fields, "revoked_at_micros")?,
            legacy: matches!(fields.get("legacy"), Some(CapsuleValue::Bool(true))),
        },
    })
}

impl CapsuleKeyDirectory<'_> {
    fn query(&self, predicate: QueryPredicate) -> PortResult<Vec<CapsuleEnvelope>> {
        Ok(self
            .repository
            .query(&CapsuleQuery {
                kind: CapsuleKind(IDENTITY_KEY.to_owned()),
                predicate: Some(predicate),
                projection: BTreeSet::new(),
                sort: Vec::new(),
                aggregates: Vec::new(),
                traversals: Vec::new(),
                limit: MAX_QUERY_LIMIT,
                cursor: None,
            })
            .map_err(port_error)?
            .into_iter()
            .filter(|capsule| !capsule.tombstone)
            .collect())
    }

    fn capsule(&self, key_id: &str) -> PortResult<Option<CapsuleEnvelope>> {
        Ok(self
            .query(equal("key_id", CapsuleValue::Text(key_id.to_owned())))?
            .into_iter()
            .next())
    }

    /// Set a time field to the earlier of its value and `at_millis`.
    fn keep_earliest(&self, key_id: &str, field: &str, at_millis: i64) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let current = self.capsule(key_id)?.ok_or(PortError::NotFound)?;
            let mut stored = body(&current).ok_or(PortError::NotFound)?.clone();
            let earliest = match millis(&stored, field)? {
                Some(recorded) if recorded <= at_millis => return Ok(()),
                _ => at_millis,
            };
            stored.insert(field.to_owned(), micros(earliest)?);
            match self
                .repository
                .put(&next_revision(&current, stored)?, Some(current.revision))
            {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }
}

impl KeyDirectory for CapsuleKeyDirectory<'_> {
    fn key(&self, key_id: &str) -> PortResult<Option<IdentityKey>> {
        self.capsule(key_id)?.as_ref().map(record).transpose()
    }

    fn epochs(&self, subject: &Subject, purpose: KeyPurpose) -> PortResult<Vec<IdentityKey>> {
        let mut keys = self
            .query(QueryPredicate::And {
                predicates: vec![
                    equal(
                        "subject_kind",
                        CapsuleValue::Text(subject.kind.as_str().to_owned()),
                    ),
                    equal(
                        "subject_id",
                        CapsuleValue::Bytes(subject.id.as_bytes().to_vec()),
                    ),
                    equal("purpose", CapsuleValue::Text(purpose.as_str().to_owned())),
                ],
            })?
            .iter()
            .map(record)
            .collect::<PortResult<Vec<_>>>()?;
        keys.sort_by_key(|key| key.epoch.epoch);
        Ok(keys)
    }

    fn register(&self, key: &IdentityKey) -> PortResult<()> {
        // The unique indexes on the key ID and on (subject, purpose, epoch) refuse a
        // duplicate atomically; the provider reports it as a conflict.
        self.repository
            .put(
                &new_capsule(
                    *uuid::Uuid::now_v7().as_bytes(),
                    IDENTITY_KEY,
                    StorageClass::Core,
                    OwnerScope::Global,
                    Vec::new(),
                    fields(key)?,
                )?,
                None,
            )
            .map_err(port_error)
    }

    fn retire(&self, key_id: &str, at_millis: i64) -> PortResult<()> {
        self.keep_earliest(key_id, "retired_at_micros", at_millis)
    }

    fn revoke(&self, key_id: &str, at_millis: i64) -> PortResult<()> {
        self.keep_earliest(key_id, "revoked_at_micros", at_millis)
    }
}
