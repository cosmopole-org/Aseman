//! Capsule helpers shared by the typed repositories: construction, tombstones,
//! legacy identities, keyset scans, and identity lookups.

use crate::CapsuleStore;
use crate::store::{body, now_micros, port_error};
use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery, CapsuleRelationship,
    CapsuleValue, ComparisonOperator, MAX_QUERY_LIMIT, OwnerScope, QueryPredicate, QuerySort,
    SortDirection, StorageClass,
};
use aseman_contracts::legacy_realtime::deterministic_legacy_capsule_id;
use aseman_ports::{PortError, PortResult};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const LEGACY_IDENTITY: &str = "core.legacy_identity";
pub(crate) const MAX_CAS_ATTEMPTS: usize = 8;

pub(crate) fn kind(name: &str) -> CapsuleKind {
    CapsuleKind(name.to_owned())
}

pub(crate) fn text(body: &BTreeMap<String, CapsuleValue>, field: &str) -> String {
    match body.get(field) {
        Some(CapsuleValue::Text(value)) => value.clone(),
        _ => String::new(),
    }
}

pub(crate) fn failed(error: impl ToString) -> PortError {
    PortError::Failed(error.to_string())
}

/// A new revision-1 capsule, sealed.
pub(crate) fn new_capsule(
    id: [u8; 16],
    kind_name: &str,
    storage_class: StorageClass,
    owner_scope: OwnerScope,
    relationships: Vec<CapsuleRelationship>,
    fields: BTreeMap<String, CapsuleValue>,
) -> PortResult<CapsuleEnvelope> {
    let now = now_micros();
    CapsuleEnvelope {
        encoding_version: 1,
        id: CapsuleId(id),
        kind: kind(kind_name),
        storage_class,
        owner_scope,
        schema_version: 1,
        revision: 1,
        created_at_micros: now,
        updated_at_micros: now,
        previous_integrity: None,
        integrity_hash: CapsuleDigest {
            algorithm: "sha2-256".to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: false,
        relationships,
        body: Some(CapsuleValue::Object(fields)),
    }
    .seal()
    .map_err(failed)
}

/// The tombstone revision of `current`.
pub(crate) fn tombstone(current: &CapsuleEnvelope) -> PortResult<CapsuleEnvelope> {
    CapsuleEnvelope {
        revision: current.revision + 1,
        previous_integrity: Some(current.integrity_hash.clone()),
        updated_at_micros: now_micros().max(current.updated_at_micros),
        tombstone: true,
        body: None,
        ..current.clone()
    }
    .seal()
    .map_err(failed)
}

pub(crate) fn relationship(name: &str, target_kind: &str, target: [u8; 16]) -> CapsuleRelationship {
    CapsuleRelationship {
        name: name.to_owned(),
        target_kind: kind(target_kind),
        target_id: CapsuleId(target),
    }
}

pub(crate) fn legacy_identity(
    family: &str,
    legacy_id: &str,
    target_kind: &str,
) -> PortResult<CapsuleEnvelope> {
    let target = deterministic_legacy_capsule_id(family, legacy_id.as_bytes());
    new_capsule(
        deterministic_legacy_capsule_id(
            "LegacyIdentity",
            format!("{family}\0{legacy_id}").as_bytes(),
        ),
        LEGACY_IDENTITY,
        StorageClass::Core,
        OwnerScope::Global,
        Vec::new(),
        BTreeMap::from([
            ("family".to_owned(), CapsuleValue::Text(family.to_owned())),
            (
                "legacy_id".to_owned(),
                CapsuleValue::Text(legacy_id.to_owned()),
            ),
            (
                "target_kind".to_owned(),
                CapsuleValue::Text(target_kind.to_owned()),
            ),
            ("target_id".to_owned(), CapsuleValue::Bytes(target.to_vec())),
        ]),
    )
}

pub(crate) fn equal(field: &str, value: CapsuleValue) -> QueryPredicate {
    QueryPredicate::Compare {
        field: field.to_owned(),
        operator: ComparisonOperator::Equal,
        value,
    }
}

/// The repository-bound helpers.
pub(crate) struct Capsules<'a>(pub(crate) &'a dyn CapsuleStore);

impl Capsules<'_> {
    pub(crate) fn get(&self, kind_name: &str, id: [u8; 16]) -> PortResult<Option<CapsuleEnvelope>> {
        self.0
            .get(&kind(kind_name), &CapsuleId(id))
            .map_err(port_error)
    }

    pub(crate) fn live(
        &self,
        kind_name: &str,
        id: [u8; 16],
    ) -> PortResult<Option<CapsuleEnvelope>> {
        Ok(self
            .get(kind_name, id)?
            .filter(|capsule| !capsule.tombstone))
    }

    /// Every live capsule of `kind_name` matching `predicates`, paged by keyset on
    /// the unique text field `key`. The provider's collation orders the pages; callers
    /// re-sort by bytes wherever legacy order matters.
    pub(crate) fn scan(
        &self,
        kind_name: &str,
        predicates: Vec<QueryPredicate>,
        key: &str,
    ) -> PortResult<Vec<CapsuleEnvelope>> {
        let mut rows = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut page_predicates = predicates.clone();
            if let Some(last) = &after {
                page_predicates.push(QueryPredicate::Compare {
                    field: key.to_owned(),
                    operator: ComparisonOperator::GreaterThan,
                    value: CapsuleValue::Text(last.clone()),
                });
            }
            let page = self
                .0
                .query(&CapsuleQuery {
                    kind: kind(kind_name),
                    predicate: match page_predicates.len() {
                        0 => None,
                        1 => page_predicates.pop(),
                        _ => Some(QueryPredicate::And {
                            predicates: page_predicates,
                        }),
                    },
                    projection: BTreeSet::new(),
                    sort: vec![QuerySort {
                        field: key.to_owned(),
                        direction: SortDirection::Ascending,
                    }],
                    aggregates: Vec::new(),
                    traversals: Vec::new(),
                    limit: MAX_QUERY_LIMIT,
                    cursor: None,
                })
                .map_err(port_error)?;
            let full = page.len() == MAX_QUERY_LIMIT as usize;
            after = page
                .iter()
                .rev()
                .find_map(|capsule| body(capsule).map(|fields| text(fields, key)));
            rows.extend(page.into_iter().filter(|capsule| !capsule.tombstone));
            if !full || after.is_none() {
                return Ok(rows);
            }
        }
    }

    /// Canonical ID -> legacy identity, for every identity of `family`.
    pub(crate) fn legacy_ids(&self, family: &str) -> PortResult<BTreeMap<[u8; 16], String>> {
        let mut map = BTreeMap::new();
        for row in self.scan(
            LEGACY_IDENTITY,
            vec![equal("family", CapsuleValue::Text(family.to_owned()))],
            "legacy_id",
        )? {
            let Some(fields) = body(&row) else { continue };
            if let Some(CapsuleValue::Bytes(target)) = fields.get("target_id")
                && let Ok(target) = <[u8; 16]>::try_from(target.as_slice())
            {
                map.insert(target, text(fields, "legacy_id"));
            }
        }
        Ok(map)
    }

    /// The legacy identity of one canonical ID, by the unique target index.
    pub(crate) fn legacy_id_of(&self, target_kind: &str, target: [u8; 16]) -> PortResult<String> {
        let rows = self
            .0
            .query(&CapsuleQuery {
                kind: kind(LEGACY_IDENTITY),
                predicate: Some(QueryPredicate::And {
                    predicates: vec![
                        equal("target_kind", CapsuleValue::Text(target_kind.to_owned())),
                        equal("target_id", CapsuleValue::Bytes(target.to_vec())),
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
            .map(|fields| text(fields, "legacy_id"))
        {
            Some(legacy_id) if !legacy_id.is_empty() => Ok(legacy_id),
            _ => Err(failed(format!("{target_kind} has no legacy identity"))),
        }
    }
}

/// An ADR 0016 document family: a document capsule bound to one subject capsule.
pub(crate) struct DocumentFamily {
    pub(crate) kind: &'static str,
    /// The deterministic-ID family of the document capsule.
    pub(crate) family: &'static str,
    /// The legacy key prefix, used to name the document in errors and digests.
    pub(crate) key_prefix: &'static str,
    pub(crate) subject_relationship: &'static str,
    pub(crate) subject_kind: &'static str,
    pub(crate) subject_family: &'static str,
}

fn stored_document(
    capsule: &CapsuleEnvelope,
) -> PortResult<serde_json::Map<String, serde_json::Value>> {
    match body(capsule)
        .and_then(|fields| fields.get("document"))
        .map(aseman_contracts::legacy_documents::capsule_value_to_json)
        .transpose()
        .map_err(failed)?
    {
        Some(serde_json::Value::Object(document)) => Ok(document),
        _ => Err(failed("document capsule has no document")),
    }
}

impl Capsules<'_> {
    /// The object at a legacy dotted `path` under `metadata`, as legacy `get_json`.
    pub(crate) fn document_at(
        &self,
        family: &DocumentFamily,
        legacy_id: &str,
        path: &str,
    ) -> PortResult<Option<String>> {
        let id = deterministic_legacy_capsule_id(family.family, legacy_id.as_bytes());
        let Some(capsule) = self.live(family.kind, id)? else {
            return Ok(None);
        };
        let document = stored_document(&capsule)?;
        aseman_contracts::legacy_documents::legacy_document_object_at("metadata", &document, path)
            .map(|object| serde_json::to_string(object).map_err(failed))
            .transpose()
    }

    /// Deep-merge a JSON object into the document, as legacy `put_json(.., true)`.
    pub(crate) fn merge_document(
        &self,
        family: &DocumentFamily,
        legacy_id: &str,
        document: &str,
    ) -> PortResult<()> {
        let Ok(serde_json::Value::Object(incoming)) = serde_json::from_str(document) else {
            return Err(failed("metadata must be a JSON object"));
        };
        let id = deterministic_legacy_capsule_id(family.family, legacy_id.as_bytes());
        let key = format!("{}{legacy_id}", family.key_prefix);
        let subject = deterministic_legacy_capsule_id(family.subject_family, legacy_id.as_bytes());
        for _ in 0..MAX_CAS_ATTEMPTS {
            let written = match self.get(family.kind, id)? {
                Some(current) => {
                    let mut merged = if current.tombstone {
                        serde_json::Map::new()
                    } else {
                        stored_document(&current)?
                    };
                    aseman_contracts::legacy_documents::merge_legacy_objects(
                        &mut merged,
                        &incoming,
                    );
                    let fields = aseman_contracts::legacy_documents::legacy_document_fields(
                        &key, "metadata", &merged,
                    )
                    .map_err(failed)?;
                    let next = crate::store::next_revision(&current, fields)?;
                    self.0.put(&next, Some(current.revision))
                }
                None => {
                    let owner = self
                        .live(family.subject_kind, subject)?
                        .ok_or(PortError::NotFound)?
                        .owner_scope;
                    let fields = aseman_contracts::legacy_documents::legacy_document_fields(
                        &key, "metadata", &incoming,
                    )
                    .map_err(failed)?;
                    self.0.put(
                        &new_capsule(
                            id,
                            family.kind,
                            StorageClass::Core,
                            owner,
                            vec![relationship(
                                family.subject_relationship,
                                family.subject_kind,
                                subject,
                            )],
                            fields,
                        )?,
                        None,
                    )
                }
            };
            match written {
                Err(crate::CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }

    /// Tombstone the document. Deleting an absent document succeeds.
    pub(crate) fn delete_document(
        &self,
        family: &DocumentFamily,
        legacy_id: &str,
    ) -> PortResult<()> {
        let id = deterministic_legacy_capsule_id(family.family, legacy_id.as_bytes());
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some(current) = self.live(family.kind, id)? else {
                return Ok(());
            };
            match self.0.put(&tombstone(&current)?, Some(current.revision)) {
                Err(crate::CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }
}
