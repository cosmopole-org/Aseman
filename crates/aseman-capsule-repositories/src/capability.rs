//! Capability grants on the capsule protocol (A403): one `core.capability_grant` capsule
//! per grant, its capsule ID the grant ID. Grants are never deleted; revocation is a
//! revision.

use crate::store::{body, next_revision, port_error};
use crate::support::{Capsules, MAX_CAS_ATTEMPTS, equal, failed, new_capsule, text};
use crate::{CapsuleStore, CapsuleStoreError};
use aseman_contracts::capsule::{
    CapsuleEnvelope, CapsuleKind, CapsuleQuery, CapsuleValue, MAX_QUERY_LIMIT, OwnerScope,
    QueryPredicate, StorageClass,
};
use aseman_domain::Uuid;
use aseman_domain::capability::{Grant, ResourceSelector};
use aseman_domain::identity::{Subject, SubjectKind};
use aseman_ports::{GrantStore, PortError, PortResult};
use std::collections::{BTreeMap, BTreeSet};

const GRANT: &str = "core.capability_grant";

/// The grant store over any [`CapsuleStore`].
pub struct CapsuleGrantStore<'a> {
    pub repository: &'a dyn CapsuleStore,
}

fn micros(millis: i64) -> PortResult<CapsuleValue> {
    millis
        .checked_mul(1_000)
        .map(CapsuleValue::Integer)
        .ok_or_else(|| failed("grant time overflows microseconds"))
}

/// An action set as a document: `{"names": [...]}` (documents hold objects).
fn names(values: &BTreeSet<String>) -> CapsuleValue {
    CapsuleValue::Object(BTreeMap::from([(
        "names".to_owned(),
        CapsuleValue::Array(values.iter().cloned().map(CapsuleValue::Text).collect()),
    )]))
}

fn fields(grant: &Grant) -> PortResult<BTreeMap<String, CapsuleValue>> {
    let mut fields = BTreeMap::from([
        (
            "subject_kind".to_owned(),
            CapsuleValue::Text(grant.subject.kind.as_str().to_owned()),
        ),
        (
            "subject_id".to_owned(),
            CapsuleValue::Bytes(grant.subject.id.as_bytes().to_vec()),
        ),
        (
            "issuer_kind".to_owned(),
            CapsuleValue::Text(grant.issuer.kind.as_str().to_owned()),
        ),
        (
            "issuer_id".to_owned(),
            CapsuleValue::Bytes(grant.issuer.id.as_bytes().to_vec()),
        ),
        ("actions".to_owned(), names(&grant.actions)),
        (
            "resource_kind".to_owned(),
            CapsuleValue::Text(grant.resource.kind().to_owned()),
        ),
        (
            "delegable_actions".to_owned(),
            names(&grant.delegable_actions),
        ),
        (
            "max_depth".to_owned(),
            CapsuleValue::Integer(i64::from(grant.max_depth)),
        ),
        (
            "not_before_micros".to_owned(),
            micros(grant.not_before_millis)?,
        ),
        (
            "policy_version".to_owned(),
            CapsuleValue::Text(grant.policy_version.clone()),
        ),
    ]);
    if let ResourceSelector::Exact { id, .. } = &grant.resource {
        fields.insert("resource_id".to_owned(), CapsuleValue::Text(id.clone()));
    }
    if let Some(parent) = grant.parent {
        fields.insert(
            "parent_id".to_owned(),
            CapsuleValue::Bytes(parent.as_bytes().to_vec()),
        );
    }
    for (name, value) in [
        ("expires_at_micros", grant.expires_at_millis),
        ("revoked_at_micros", grant.revoked_at_millis),
    ] {
        if let Some(value) = value {
            fields.insert(name.to_owned(), micros(value)?);
        }
    }
    Ok(fields)
}

fn uuid_field(fields: &BTreeMap<String, CapsuleValue>, name: &str) -> PortResult<Option<Uuid>> {
    match fields.get(name) {
        None | Some(CapsuleValue::Null) => Ok(None),
        Some(CapsuleValue::Bytes(bytes)) => Uuid::from_slice(bytes).map(Some).map_err(failed),
        Some(_) => Err(failed(format!("grant {name} is not an identifier"))),
    }
}

fn subject(fields: &BTreeMap<String, CapsuleValue>, prefix: &str) -> PortResult<Subject> {
    let kind = text(fields, &format!("{prefix}_kind"));
    let kind = SubjectKind::ALL
        .into_iter()
        .find(|candidate| candidate.as_str() == kind)
        .ok_or_else(|| failed(format!("unknown subject kind {kind}")))?;
    let id = uuid_field(fields, &format!("{prefix}_id"))?
        .ok_or_else(|| failed(format!("grant has no {prefix}")))?;
    Ok(Subject { kind, id })
}

fn name_set(fields: &BTreeMap<String, CapsuleValue>, name: &str) -> PortResult<BTreeSet<String>> {
    let document = match fields.get(name) {
        Some(CapsuleValue::Object(document)) => document,
        _ => return Err(failed(format!("grant has no {name}"))),
    };
    match document.get("names") {
        Some(CapsuleValue::Array(values)) => values
            .iter()
            .map(|value| match value {
                CapsuleValue::Text(text) => Ok(text.clone()),
                _ => Err(failed(format!("grant {name} holds a non-name"))),
            })
            .collect(),
        _ => Err(failed(format!("grant has no {name}"))),
    }
}

fn millis(fields: &BTreeMap<String, CapsuleValue>, name: &str) -> PortResult<Option<i64>> {
    match fields.get(name) {
        None | Some(CapsuleValue::Null) => Ok(None),
        Some(CapsuleValue::Integer(micros)) => Ok(Some(micros / 1_000)),
        Some(_) => Err(failed(format!("grant {name} is not a time"))),
    }
}

fn record(capsule: &CapsuleEnvelope) -> PortResult<Grant> {
    let fields = body(capsule).ok_or(PortError::NotFound)?;
    let kind = text(fields, "resource_kind");
    let resource = match fields.get("resource_id") {
        Some(CapsuleValue::Text(id)) => ResourceSelector::Exact {
            kind,
            id: id.clone(),
        },
        _ => ResourceSelector::AnyOfKind { kind },
    };
    Ok(Grant {
        id: Uuid::from_bytes(capsule.id.0),
        subject: subject(fields, "subject")?,
        issuer: subject(fields, "issuer")?,
        actions: name_set(fields, "actions")?,
        resource,
        delegable_actions: name_set(fields, "delegable_actions")?,
        max_depth: match fields.get("max_depth") {
            Some(CapsuleValue::Integer(depth)) => u32::try_from(*depth).map_err(failed)?,
            _ => return Err(failed("grant has no depth")),
        },
        parent: uuid_field(fields, "parent_id")?,
        not_before_millis: millis(fields, "not_before_micros")?
            .ok_or_else(|| failed("grant has no start"))?,
        expires_at_millis: millis(fields, "expires_at_micros")?,
        revoked_at_millis: millis(fields, "revoked_at_micros")?,
        policy_version: text(fields, "policy_version"),
    })
}

impl CapsuleGrantStore<'_> {
    fn query(&self, predicate: QueryPredicate) -> PortResult<Vec<Grant>> {
        let mut grants = self
            .repository
            .query(&CapsuleQuery {
                kind: CapsuleKind(GRANT.to_owned()),
                predicate: Some(predicate),
                projection: BTreeSet::new(),
                sort: Vec::new(),
                aggregates: Vec::new(),
                traversals: Vec::new(),
                limit: MAX_QUERY_LIMIT,
                cursor: None,
            })
            .map_err(port_error)?
            .iter()
            .filter(|capsule| !capsule.tombstone)
            .map(record)
            .collect::<PortResult<Vec<_>>>()?;
        grants.sort_by_key(|grant| grant.id);
        Ok(grants)
    }
}

impl GrantStore for CapsuleGrantStore<'_> {
    fn grant(&self, id: Uuid) -> PortResult<Option<Grant>> {
        Capsules(self.repository)
            .live(GRANT, *id.as_bytes())?
            .as_ref()
            .map(record)
            .transpose()
    }

    fn grants_of(&self, subject: &Subject) -> PortResult<Vec<Grant>> {
        self.query(QueryPredicate::And {
            predicates: vec![
                equal(
                    "subject_kind",
                    CapsuleValue::Text(subject.kind.as_str().to_owned()),
                ),
                equal(
                    "subject_id",
                    CapsuleValue::Bytes(subject.id.as_bytes().to_vec()),
                ),
            ],
        })
    }

    fn children(&self, parent: Uuid) -> PortResult<Vec<Grant>> {
        self.query(equal(
            "parent_id",
            CapsuleValue::Bytes(parent.as_bytes().to_vec()),
        ))
    }

    fn put(&self, grant: &Grant) -> PortResult<()> {
        self.repository
            .put(
                &new_capsule(
                    *grant.id.as_bytes(),
                    GRANT,
                    StorageClass::Core,
                    OwnerScope::Global,
                    Vec::new(),
                    fields(grant)?,
                )?,
                None,
            )
            .map_err(port_error)
    }

    fn revoke(&self, id: Uuid, at_millis: i64) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let current = Capsules(self.repository)
                .live(GRANT, *id.as_bytes())?
                .ok_or(PortError::NotFound)?;
            let mut stored = body(&current).ok_or(PortError::NotFound)?.clone();
            if millis(&stored, "revoked_at_micros")?.is_some_and(|revoked| revoked <= at_millis) {
                return Ok(());
            }
            stored.insert("revoked_at_micros".to_owned(), micros(at_millis)?);
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
