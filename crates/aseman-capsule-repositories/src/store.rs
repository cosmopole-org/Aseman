//! Store repositories on the capsule protocol (RL-004 strangler, target side): stores
//! and memberships as core capsules, and signals as `realtime.event` streams encoded
//! exactly like migrated history. Works with any provider behind [`CapsuleStore`].

use crate::support::{
    Capsules, DocumentFamily, MAX_CAS_ATTEMPTS, failed, legacy_identity, new_capsule, tombstone,
};
use crate::{CapsuleStore, CapsuleStoreError};
use aseman_contracts::capsule::OwnerScope;
use aseman_contracts::capsule::{
    CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery, CapsuleRelationship, CapsuleValue,
    ComparisonOperator, MAX_QUERY_LIMIT, QueryPredicate, QuerySort, SortDirection, StorageClass,
};
use aseman_contracts::legacy_realtime::{
    SignalStreamPolicy, StoreSignalPayload, decode_store_signal, deterministic_legacy_capsule_id,
    store_signal_event, store_signal_stream,
};
use aseman_domain::signal_tags::LogQuery;
use aseman_domain::store::{StoreRecord, StoreSignal};
use aseman_domain::store_permissions::StorePermissions;
use aseman_ports::{PortError, PortResult, SignalLog, StoreAccess, StoreDirectory, StoreMetadata};
use std::collections::{BTreeMap, BTreeSet};

/// Resolves each store stream's authorization scope and retention (P7-04 rule).
pub type StreamPolicyResolver = dyn Fn(&str) -> SignalStreamPolicy + Send + Sync;

pub struct CapsuleStorePorts<'a> {
    pub repository: &'a dyn CapsuleStore,
    pub stream_policy: &'a StreamPolicyResolver,
}

pub(crate) fn port_error(error: CapsuleStoreError) -> PortError {
    match error {
        CapsuleStoreError::Conflict => PortError::Conflict,
        CapsuleStoreError::Failed(message) => PortError::Failed(message),
    }
}

pub(crate) fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
        })
}

pub(crate) fn body(capsule: &CapsuleEnvelope) -> Option<&BTreeMap<String, CapsuleValue>> {
    match &capsule.body {
        Some(CapsuleValue::Object(body)) if !capsule.tombstone => Some(body),
        _ => None,
    }
}

/// The next live revision of `current` with `body`, chained and sealed. A tombstone
/// revived this way continues its revision chain.
pub(crate) fn next_revision(
    current: &CapsuleEnvelope,
    body: BTreeMap<String, CapsuleValue>,
) -> PortResult<CapsuleEnvelope> {
    CapsuleEnvelope {
        revision: current.revision + 1,
        previous_integrity: Some(current.integrity_hash.clone()),
        updated_at_micros: now_micros().max(current.updated_at_micros),
        // A revision with a body is live, which also revives a tombstone.
        tombstone: false,
        body: Some(CapsuleValue::Object(body)),
        ..current.clone()
    }
    .seal()
    .map_err(|error| PortError::Failed(error.to_string()))
}

impl CapsuleStorePorts<'_> {
    fn get(
        &self,
        kind: &str,
        family: &str,
        legacy_id: &str,
    ) -> PortResult<Option<CapsuleEnvelope>> {
        self.repository
            .get(
                &CapsuleKind(kind.to_owned()),
                &CapsuleId(deterministic_legacy_capsule_id(
                    family,
                    legacy_id.as_bytes(),
                )),
            )
            .map_err(port_error)
    }

    /// Apply `update` as a compare-and-swap revision, retrying on conflict.
    fn update(
        &self,
        kind: &str,
        family: &str,
        legacy_id: &str,
        update: impl Fn(&mut BTreeMap<String, CapsuleValue>),
    ) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let current = self
                .get(kind, family, legacy_id)?
                .ok_or(PortError::NotFound)?;
            let mut next = body(&current).cloned().ok_or(PortError::NotFound)?;
            update(&mut next);
            let revision = next_revision(&current, next)?;
            match self.repository.put(&revision, Some(current.revision)) {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }
}

/// The body of a `core.store` capsule, as the A308 export writes it.
fn store_fields(record: &StoreRecord) -> BTreeMap<String, CapsuleValue> {
    BTreeMap::from([
        ("is_public".to_owned(), CapsuleValue::Bool(record.is_public)),
        (
            "member_count".to_owned(),
            CapsuleValue::Integer(record.member_count.max(1)),
        ),
        (
            "persistent_history".to_owned(),
            CapsuleValue::Bool(record.persistent_history),
        ),
        (
            "signal_count".to_owned(),
            CapsuleValue::Integer(record.signal_count),
        ),
        ("tag".to_owned(), CapsuleValue::Text(record.tag.clone())),
    ])
}

/// The store's relationships: its creator, and its parent when it has one.
fn store_relationships(record: &StoreRecord, creator: CapsuleId) -> Vec<CapsuleRelationship> {
    let mut relationships = vec![CapsuleRelationship {
        name: "creature".to_owned(),
        target_kind: CapsuleKind("core.creature".to_owned()),
        target_id: creator,
    }];
    if !record.parent_id.is_empty() {
        relationships.push(CapsuleRelationship {
            name: "parent".to_owned(),
            target_kind: CapsuleKind("core.store".to_owned()),
            target_id: CapsuleId(deterministic_legacy_capsule_id(
                "Store",
                record.parent_id.as_bytes(),
            )),
        });
    }
    relationships
}

const STORE_METADATA: DocumentFamily = DocumentFamily {
    kind: "core.store_metadata",
    family: "StoreMetadata",
    key_prefix: "StoreMeta::",
    subject_relationship: "store",
    subject_kind: "core.store",
    subject_family: "Store",
};

impl StoreDirectory for CapsuleStorePorts<'_> {
    fn store(&self, store_id: &str) -> PortResult<Option<StoreRecord>> {
        let Some(capsule) = self.get("core.store", "Store", store_id)? else {
            return Ok(None);
        };
        let Some(body) = body(&capsule) else {
            return Ok(None);
        };
        let integer = |field: &str| match body.get(field) {
            Some(CapsuleValue::Integer(value)) => *value,
            _ => 0,
        };
        let parent_id = match capsule
            .relationships
            .iter()
            .find(|relationship| relationship.name == "parent")
        {
            Some(parent) => self.legacy_id("core.store", parent.target_id.0)?,
            None => String::new(),
        };
        Ok(Some(StoreRecord {
            id: store_id.to_owned(),
            persistent_history: body.get("persistent_history") == Some(&CapsuleValue::Bool(true)),
            signal_count: integer("signal_count"),
            tag: match body.get("tag") {
                Some(CapsuleValue::Text(tag)) => tag.clone(),
                _ => String::new(),
            },
            parent_id,
            is_public: body.get("is_public") == Some(&CapsuleValue::Bool(true)),
            member_count: integer("member_count"),
        }))
    }

    fn record_signal(&self, store_id: &str) -> PortResult<()> {
        self.update("core.store", "Store", store_id, |body| {
            let count = match body.get("signal_count") {
                Some(CapsuleValue::Integer(count)) => *count,
                _ => 0,
            };
            body.insert(
                "signal_count".to_owned(),
                CapsuleValue::Integer(count.saturating_add(1)),
            );
        })
    }

    fn stores(&self, offset: i64, count: Option<i64>) -> PortResult<Vec<StoreRecord>> {
        let mut identities = Capsules(self.repository)
            .legacy_ids("Store")?
            .into_values()
            .collect::<Vec<_>>();
        // Legacy lists objects in identity byte order.
        identities.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        let mut records = Vec::new();
        for legacy_id in identities {
            if let Some(record) = self.store(&legacy_id)? {
                records.push(record);
            }
        }
        Ok(aseman_domain::creature::legacy_page(records, offset, count))
    }

    fn create_store(&self, record: &StoreRecord, creator_id: &str) -> PortResult<()> {
        let id = CapsuleId(deterministic_legacy_capsule_id(
            "Store",
            record.id.as_bytes(),
        ));
        let existing = self
            .repository
            .get(&CapsuleKind("core.store".to_owned()), &id)
            .map_err(port_error)?;
        if existing.as_ref().is_some_and(|capsule| !capsule.tombstone) {
            return Err(PortError::Conflict);
        }
        // The creator owns the store; it must be a live creature, as in the export.
        let creator = deterministic_legacy_capsule_id("Creature", creator_id.as_bytes());
        if self
            .get("core.creature", "Creature", creator_id)?
            .is_none_or(|capsule| capsule.tombstone)
        {
            return Err(failed(format!("store creator {creator_id} does not exist")));
        }
        let writes = match existing {
            // Registering a deleted store again revives it, as legacy allows.
            Some(tombstoned) => vec![(
                CapsuleEnvelope {
                    owner_scope: OwnerScope::Creature(creator),
                    relationships: store_relationships(record, CapsuleId(creator)),
                    ..next_revision(&tombstoned, store_fields(record))?
                }
                .seal()
                .map_err(failed)?,
                Some(tombstoned.revision),
            )],
            None => vec![
                (
                    new_capsule(
                        id.0,
                        "core.store",
                        StorageClass::Core,
                        OwnerScope::Creature(creator),
                        store_relationships(record, CapsuleId(creator)),
                        store_fields(record),
                    )?,
                    None,
                ),
                (legacy_identity("Store", &record.id, "core.store")?, None),
            ],
        };
        self.repository.put_all(&writes).map_err(port_error)
    }

    fn update_store(&self, record: &StoreRecord) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let current = self
                .get("core.store", "Store", &record.id)?
                .filter(|capsule| !capsule.tombstone)
                .ok_or(PortError::NotFound)?;
            let creator = current
                .relationships
                .iter()
                .find(|relationship| relationship.name == "creature")
                .map(|relationship| relationship.target_id.clone())
                .ok_or_else(|| failed("store has no creator"))?;
            let next = CapsuleEnvelope {
                relationships: store_relationships(record, creator),
                ..next_revision(&current, store_fields(record))?
            }
            .seal()
            .map_err(failed)?;
            match self.repository.put(&next, Some(current.revision)) {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }

    fn delete_store(&self, store_id: &str) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some(current) = self
                .get("core.store", "Store", store_id)?
                .filter(|capsule| !capsule.tombstone)
            else {
                return Ok(());
            };
            match self
                .repository
                .put(&tombstone(&current)?, Some(current.revision))
            {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }

    fn release_creator(&self, _store_id: &str, _creator_id: &str) -> PortResult<()> {
        // The store keeps its creator relationship to the tombstoned creature.
        Ok(())
    }
}

impl StoreMetadata for CapsuleStorePorts<'_> {
    fn store_metadata(&self, store_id: &str, path: &str) -> PortResult<Option<String>> {
        Capsules(self.repository).document_at(&STORE_METADATA, store_id, path)
    }

    fn merge_store_metadata(&self, store_id: &str, document: &str) -> PortResult<()> {
        Capsules(self.repository).merge_document(&STORE_METADATA, store_id, document)
    }

    fn delete_store_metadata(&self, store_id: &str) -> PortResult<()> {
        Capsules(self.repository).delete_document(&STORE_METADATA, store_id)
    }
}

impl StoreAccess for CapsuleStorePorts<'_> {
    fn permissions(&self, store_id: &str, member_id: &str) -> PortResult<StorePermissions> {
        let identity = format!("{store_id}\0{member_id}");
        let membership = self.get("core.store_membership", "StoreMembership", &identity)?;
        Ok(
            match membership
                .as_ref()
                .and_then(body)
                .and_then(|body| body.get("permissions"))
            {
                Some(CapsuleValue::Text(permissions)) => StorePermissions::parse(permissions),
                _ => StorePermissions::default(),
            },
        )
    }

    fn set_permissions(
        &self,
        store_id: &str,
        member_id: &str,
        permissions: StorePermissions,
    ) -> PortResult<()> {
        let identity = format!("{store_id}\0{member_id}");
        let encoded = CapsuleValue::Text(permissions.encode());
        let existing = self.get("core.store_membership", "StoreMembership", &identity)?;
        if existing.as_ref().is_some_and(|capsule| !capsule.tombstone) {
            return self.update(
                "core.store_membership",
                "StoreMembership",
                &identity,
                |body| {
                    body.insert("permissions".to_owned(), encoded.clone());
                },
            );
        }
        let store = self
            .get("core.store", "Store", store_id)?
            .ok_or(PortError::NotFound)?;
        // ADR 0018: a local creature, then a local program, else a remote principal.
        let (member_kind, member_relationship) =
            if self.get("core.creature", "Creature", member_id)?.is_some() {
                ("creature", Some(("creature", "core.creature", "Creature")))
            } else if self.get("core.program", "Program", member_id)?.is_some() {
                ("program", Some(("program", "core.program", "Program")))
            } else {
                ("remote_principal", None)
            };
        let mut relationships = vec![CapsuleRelationship {
            name: "store".to_owned(),
            target_kind: CapsuleKind("core.store".to_owned()),
            target_id: store.id.clone(),
        }];
        if let Some((name, kind, family)) = member_relationship {
            relationships.push(CapsuleRelationship {
                name: name.to_owned(),
                target_kind: CapsuleKind(kind.to_owned()),
                target_id: CapsuleId(deterministic_legacy_capsule_id(
                    family,
                    member_id.as_bytes(),
                )),
            });
        }
        let now = now_micros();
        let body = BTreeMap::from([
            (
                "member_kind".to_owned(),
                CapsuleValue::Text(member_kind.to_owned()),
            ),
            (
                "member_ref".to_owned(),
                CapsuleValue::Text(member_id.to_owned()),
            ),
            ("permissions".to_owned(), encoded),
            ("joined_at_micros".to_owned(), CapsuleValue::Integer(now)),
        ]);
        // Re-joining after `leave` revives the tombstone as its next revision.
        if let Some(tombstone) = existing {
            let revived = CapsuleEnvelope {
                relationships,
                ..next_revision(&tombstone, body)?
            }
            .seal()
            .map_err(|error| PortError::Failed(error.to_string()))?;
            return self
                .repository
                .put(&revived, Some(tombstone.revision))
                .map_err(port_error);
        }
        let capsule = CapsuleEnvelope {
            encoding_version: 1,
            id: CapsuleId(deterministic_legacy_capsule_id(
                "StoreMembership",
                identity.as_bytes(),
            )),
            kind: CapsuleKind("core.store_membership".to_owned()),
            storage_class: StorageClass::Core,
            owner_scope: store.owner_scope.clone(),
            schema_version: 1,
            revision: 1,
            created_at_micros: now,
            updated_at_micros: now,
            previous_integrity: None,
            integrity_hash: store.integrity_hash.clone(),
            tombstone: false,
            relationships,
            body: Some(CapsuleValue::Object(body)),
        }
        .seal()
        .map_err(|error| PortError::Failed(error.to_string()))?;
        self.repository.put(&capsule, None).map_err(port_error)
    }

    fn is_member(&self, store_id: &str, member_id: &str) -> PortResult<bool> {
        let identity = format!("{store_id}\0{member_id}");
        Ok(self
            .get("core.store_membership", "StoreMembership", &identity)?
            .is_some_and(|capsule| !capsule.tombstone))
    }

    fn members(&self, store_id: &str) -> PortResult<Vec<(String, StorePermissions)>> {
        let store = CapsuleValue::Bytes(
            deterministic_legacy_capsule_id("Store", store_id.as_bytes()).to_vec(),
        );
        let rows = self
            .repository
            .query(&membership_query("store", store))
            .map_err(port_error)?;
        let mut members = rows
            .iter()
            .filter_map(body)
            .filter_map(
                |body| match (body.get("member_ref"), body.get("permissions")) {
                    (Some(CapsuleValue::Text(member)), Some(CapsuleValue::Text(permissions))) => {
                        Some((member.clone(), StorePermissions::parse(permissions)))
                    }
                    _ => None,
                },
            )
            .collect::<Vec<_>>();
        members.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(members)
    }

    fn stores_of(&self, member_id: &str) -> PortResult<Vec<String>> {
        let rows = self
            .repository
            .query(&membership_query(
                "member_ref",
                CapsuleValue::Text(member_id.to_owned()),
            ))
            .map_err(port_error)?;
        let mut stores = Vec::new();
        for membership in rows.iter().filter(|capsule| !capsule.tombstone) {
            let Some(store) = membership
                .relationships
                .iter()
                .find(|relationship| relationship.name == "store")
            else {
                continue;
            };
            stores.push(self.legacy_id("core.store", store.target_id.0)?);
        }
        stores.sort();
        Ok(stores)
    }

    fn join(
        &self,
        store_id: &str,
        member_id: &str,
        permissions: StorePermissions,
    ) -> PortResult<()> {
        self.set_permissions(store_id, member_id, permissions)
    }

    fn leave(&self, store_id: &str, member_id: &str) -> PortResult<()> {
        let identity = format!("{store_id}\0{member_id}");
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some(current) = self.get("core.store_membership", "StoreMembership", &identity)?
            else {
                return Ok(());
            };
            if current.tombstone {
                return Ok(());
            }
            let tombstone = CapsuleEnvelope {
                revision: current.revision + 1,
                previous_integrity: Some(current.integrity_hash.clone()),
                updated_at_micros: now_micros().max(current.updated_at_micros),
                tombstone: true,
                body: None,
                ..current.clone()
            }
            .seal()
            .map_err(|error| PortError::Failed(error.to_string()))?;
            match self.repository.put(&tombstone, Some(current.revision)) {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }
}

fn membership_query(field: &str, value: CapsuleValue) -> CapsuleQuery {
    CapsuleQuery {
        kind: CapsuleKind("core.store_membership".to_owned()),
        predicate: Some(QueryPredicate::Compare {
            field: field.to_owned(),
            operator: ComparisonOperator::Equal,
            value,
        }),
        projection: BTreeSet::new(),
        sort: Vec::new(),
        aggregates: Vec::new(),
        traversals: Vec::new(),
        limit: MAX_QUERY_LIMIT,
        cursor: None,
    }
}

impl CapsuleStorePorts<'_> {
    /// Resolve a capsule ID back to its legacy ID through the legacy-ID map.
    fn legacy_id(&self, kind: &str, id: [u8; 16]) -> PortResult<String> {
        let rows = self
            .repository
            .query(&CapsuleQuery {
                kind: CapsuleKind("core.legacy_identity".to_owned()),
                predicate: Some(QueryPredicate::And {
                    predicates: vec![
                        QueryPredicate::Compare {
                            field: "target_kind".to_owned(),
                            operator: ComparisonOperator::Equal,
                            value: CapsuleValue::Text(kind.to_owned()),
                        },
                        QueryPredicate::Compare {
                            field: "target_id".to_owned(),
                            operator: ComparisonOperator::Equal,
                            value: CapsuleValue::Bytes(id.to_vec()),
                        },
                    ],
                }),
                projection: BTreeSet::new(),
                sort: Vec::new(),
                aggregates: Vec::new(),
                traversals: Vec::new(),
                limit: 1,
                cursor: None,
            })
            .map_err(port_error)?;
        match rows
            .first()
            .and_then(body)
            .and_then(|body| body.get("legacy_id"))
        {
            Some(CapsuleValue::Text(legacy)) => Ok(legacy.clone()),
            _ => Err(PortError::Failed(format!("{kind} has no legacy identity"))),
        }
    }
}

fn stream_query(stream: &str, extra: Vec<QueryPredicate>, limit: u32) -> CapsuleQuery {
    let mut predicates = vec![QueryPredicate::Compare {
        field: "stream_id".to_owned(),
        operator: ComparisonOperator::Equal,
        value: CapsuleValue::Text(stream.to_owned()),
    }];
    predicates.extend(extra);
    CapsuleQuery {
        kind: CapsuleKind("realtime.event".to_owned()),
        predicate: Some(QueryPredicate::And { predicates }),
        projection: BTreeSet::new(),
        sort: vec![QuerySort {
            field: "sequence".to_owned(),
            direction: SortDirection::Descending,
        }],
        aggregates: Vec::new(),
        traversals: Vec::new(),
        limit,
        cursor: None,
    }
}

fn sequence_of(event: &CapsuleEnvelope) -> i64 {
    match body(event).and_then(|body| body.get("sequence")) {
        Some(CapsuleValue::Integer(sequence)) => *sequence,
        _ => 0,
    }
}

fn domain_signal(payload: StoreSignalPayload, occurred_at_micros: i64) -> StoreSignal {
    StoreSignal {
        id: payload.signal_id,
        store_id: payload.store_id,
        sender_id: payload.sender_id,
        data: payload.data,
        tags: payload.tags,
        time_millis: occurred_at_micros / 1_000,
        edited: payload.edited,
    }
}

impl SignalLog for CapsuleStorePorts<'_> {
    fn append(
        &self,
        store_id: &str,
        sender_id: &str,
        data: &str,
        tags: &[String],
        time_millis: i64,
    ) -> PortResult<StoreSignal> {
        let stream = store_signal_stream(store_id);
        let policy = (self.stream_policy)(store_id);
        let payload = StoreSignalPayload {
            signal_id: uuid::Uuid::now_v7().to_string(),
            store_id: store_id.to_owned(),
            sender_id: sender_id.to_owned(),
            data: data.to_owned(),
            tags: tags.to_vec(),
            edited: false,
        };
        let occurred = time_millis
            .checked_mul(1_000)
            .ok_or_else(|| PortError::Failed("signal time overflows".to_owned()))?;
        // The unique (stream_id, sequence) index turns concurrent appends into retries.
        for _ in 0..MAX_CAS_ATTEMPTS {
            let last = self
                .repository
                .query(&stream_query(&stream, Vec::new(), 1))
                .map_err(port_error)?;
            let sequence = last.first().map_or(0, sequence_of) + 1;
            let event = store_signal_event(&payload, sequence, occurred, &policy)
                .map_err(|error| PortError::Failed(error.to_string()))?;
            match self.repository.put(&event, None) {
                Err(CapsuleStoreError::Conflict) => continue,
                Err(error) => {
                    return Err(PortError::Failed(format!(
                        "signal log write failed: {error}"
                    )));
                }
                Ok(()) => return Ok(domain_signal(payload, occurred)),
            }
        }
        Err(PortError::Conflict)
    }

    fn history(&self, store_id: &str, query: &LogQuery) -> PortResult<Vec<StoreSignal>> {
        let stream = store_signal_stream(store_id);
        let wanted = usize::try_from(query.count.max(0)).unwrap_or(0);
        let mut bounds = Vec::new();
        if query.before_time > 0 {
            bounds.push(QueryPredicate::Compare {
                field: "occurred_at_micros".to_owned(),
                operator: ComparisonOperator::LessThan,
                value: CapsuleValue::Integer(query.before_time.saturating_mul(1_000)),
            });
        }
        if query.after_time > 0 {
            bounds.push(QueryPredicate::Compare {
                field: "occurred_at_micros".to_owned(),
                operator: ComparisonOperator::GreaterThan,
                value: CapsuleValue::Integer(query.after_time.saturating_mul(1_000)),
            });
        }
        let mut signals = Vec::new();
        let mut before_sequence: Option<i64> = None;
        // Tag filters are applied after decoding, so page until the request is filled.
        while signals.len() < wanted {
            let mut predicates = bounds.clone();
            if let Some(sequence) = before_sequence {
                predicates.push(QueryPredicate::Compare {
                    field: "sequence".to_owned(),
                    operator: ComparisonOperator::LessThan,
                    value: CapsuleValue::Integer(sequence),
                });
            }
            let page = self
                .repository
                .query(&stream_query(&stream, predicates, MAX_QUERY_LIMIT))
                .map_err(|error| PortError::Failed(format!("signal log read failed: {error}")))?;
            let Some(last) = page.last() else {
                break;
            };
            before_sequence = Some(sequence_of(last));
            for event in &page {
                let Some((payload, occurred)) = decode_store_signal(event) else {
                    return Err(PortError::Failed(
                        "signal log holds an undecodable event".to_owned(),
                    ));
                };
                let all = query.tags_all.iter().all(|tag| payload.tags.contains(tag));
                let any = query.tags_any.is_empty()
                    || query.tags_any.iter().any(|tag| payload.tags.contains(tag));
                if all && any && signals.len() < wanted {
                    signals.push(domain_signal(payload, occurred));
                }
            }
            if page.len() < MAX_QUERY_LIMIT as usize {
                break;
            }
        }
        Ok(signals)
    }
}
