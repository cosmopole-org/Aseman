//! Policy decision audit on the capsule protocol (`audit.event`, append-only). Each
//! actor has one stream; every event carries the next sequence and the previous
//! event's integrity, and the unique `(stream_id, sequence)` index makes concurrent
//! appends retry instead of forking the chain.

use crate::store::{body, port_error};
use crate::support::{MAX_CAS_ATTEMPTS, equal, failed, new_capsule, text};
use crate::{CapsuleStore, CapsuleStoreError};
use aseman_contracts::capsule::{
    CapsuleEnvelope, CapsuleKind, CapsuleQuery, CapsuleValue, MAX_QUERY_LIMIT, OwnerScope,
    QuerySort, SortDirection, StorageClass,
};
use aseman_domain::authority::{AuditRecord, AuditedDecision};
use aseman_ports::{DecisionAudit, PortError, PortResult};
use std::collections::{BTreeMap, BTreeSet};

const AUDIT_EVENT: &str = "audit.event";
/// The policy provider's contract version (A404 v1).
const POLICY_CONTRACT_VERSION: i64 = 1;

/// Decision audit over any [`CapsuleStore`].
pub struct CapsuleDecisionAudit<'a> {
    pub repository: &'a dyn CapsuleStore,
}

fn integer(fields: &BTreeMap<String, CapsuleValue>, name: &str) -> PortResult<i64> {
    match fields.get(name) {
        Some(CapsuleValue::Integer(value)) => Ok(*value),
        _ => Err(failed(format!("audit event has no {name}"))),
    }
}

impl CapsuleDecisionAudit<'_> {
    fn events(
        &self,
        actor: &str,
        direction: SortDirection,
        limit: u32,
    ) -> PortResult<Vec<CapsuleEnvelope>> {
        self.repository
            .query(&CapsuleQuery {
                kind: CapsuleKind(AUDIT_EVENT.to_owned()),
                predicate: Some(equal("stream_id", CapsuleValue::Text(actor.to_owned()))),
                projection: BTreeSet::new(),
                sort: vec![QuerySort {
                    field: "sequence".to_owned(),
                    direction,
                }],
                aggregates: Vec::new(),
                traversals: Vec::new(),
                limit,
                cursor: None,
            })
            .map_err(port_error)
    }
}

impl DecisionAudit for CapsuleDecisionAudit<'_> {
    fn record(&self, record: &AuditRecord) -> PortResult<u64> {
        let occurred = record
            .occurred_at_millis
            .checked_mul(1_000)
            .ok_or_else(|| failed("audit time overflows microseconds"))?;
        for _ in 0..MAX_CAS_ATTEMPTS {
            let last = self
                .events(&record.actor, SortDirection::Descending, 1)?
                .into_iter()
                .next();
            let (sequence, previous) = match &last {
                Some(event) => {
                    let fields = body(event).ok_or(PortError::NotFound)?;
                    (
                        integer(fields, "sequence")? + 1,
                        Some(event.integrity_hash.bytes.clone()),
                    )
                }
                None => (1, None),
            };
            let mut fields = BTreeMap::from([
                (
                    "stream_id".to_owned(),
                    CapsuleValue::Text(record.actor.clone()),
                ),
                ("sequence".to_owned(), CapsuleValue::Integer(sequence)),
                ("actor".to_owned(), CapsuleValue::Text(record.actor.clone())),
                (
                    "action".to_owned(),
                    CapsuleValue::Text(record.action.clone()),
                ),
                (
                    "target".to_owned(),
                    CapsuleValue::Text(record.target.clone()),
                ),
                (
                    "decision".to_owned(),
                    CapsuleValue::Text(record.decision.clone()),
                ),
                (
                    "policy_version".to_owned(),
                    CapsuleValue::Integer(POLICY_CONTRACT_VERSION),
                ),
                ("trace_id".to_owned(), CapsuleValue::Text(String::new())),
                (
                    "occurred_at_micros".to_owned(),
                    CapsuleValue::Integer(occurred),
                ),
                (
                    "details".to_owned(),
                    CapsuleValue::Bytes(record.details.as_bytes().to_vec()),
                ),
            ]);
            if let Some(previous) = previous {
                fields.insert(
                    "previous_event_integrity".to_owned(),
                    CapsuleValue::Bytes(previous),
                );
            }
            let event = new_capsule(
                *uuid::Uuid::now_v7().as_bytes(),
                AUDIT_EVENT,
                StorageClass::Audit,
                OwnerScope::Global,
                Vec::new(),
                fields,
            )?;
            match self.repository.put(&event, None) {
                Err(CapsuleStoreError::Conflict) => continue,
                other => {
                    other.map_err(port_error)?;
                    return u64::try_from(sequence).map_err(failed);
                }
            }
        }
        Err(PortError::Conflict)
    }

    fn stream(&self, actor: &str) -> PortResult<Vec<AuditedDecision>> {
        self.events(actor, SortDirection::Ascending, MAX_QUERY_LIMIT)?
            .iter()
            .map(|event| {
                let fields = body(event).ok_or(PortError::NotFound)?;
                let details = match fields.get("details") {
                    Some(CapsuleValue::Bytes(bytes)) => {
                        String::from_utf8(bytes.clone()).map_err(failed)?
                    }
                    _ => String::new(),
                };
                Ok(AuditedDecision {
                    sequence: u64::try_from(integer(fields, "sequence")?).map_err(failed)?,
                    record: AuditRecord {
                        actor: text(fields, "actor"),
                        action: text(fields, "action"),
                        target: text(fields, "target"),
                        decision: text(fields, "decision"),
                        occurred_at_millis: integer(fields, "occurred_at_micros")? / 1_000,
                        details,
                    },
                })
            })
            .collect()
    }
}
