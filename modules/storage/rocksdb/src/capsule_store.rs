//! The RocksDB provider's capsule store (ADR 0033, ADR 0034).
//!
//! Capsules live in the provider's key/value store, so on one host they are embedded
//! RocksDB and in cluster mode every write is a Raft log entry; compare-and-set and
//! unique indexes ride the same conditional batch, checked in log order on every
//! replica.
//!
//! Two layouts, like the SQL provider:
//!
//! - **Flattened** (the default, capsule mode off): a capsule is a row of keys, one per
//!   body field, holding that field's canonical value, plus one metadata key for the
//!   envelope. Rewriting a capsule deletes the keys of fields it no longer has.
//! - **Capsule mode**: one key holds the canonical envelope.
//!
//! Reads accept either layout; [`RocksDbCapsuleStore::migrate_layout`] rewrites every
//! capsule into the chosen one. Unique indexes come from the provider-neutral logical
//! schemas (`contracts/capsule/kinds`); a unique key maps the indexed values of a live
//! capsule to its id, so a second live capsule with the same values is a conflict.

use crate::{KvExpectation, LegacyKvStore, LegacyKvWrite, LegacyMigrationError};
use aseman_capsule::{CapsuleStore, CapsuleStoreError, CapsuleStoreResult};
use aseman_config::CapsuleLayout;
use aseman_contracts::capsule::{
    CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery, CapsuleValue, ComparisonOperator,
    MAX_QUERY_DEPTH, MAX_QUERY_LIMIT, ProviderCapabilities, QueryPredicate, SortDirection,
    StorageCapability, decode_canonical_value, encode_value,
};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, OnceLock};

/// The most capsules one [`CapsuleStore::put_all`] holds.
pub const MAX_TRANSACTION_CAPSULES: usize = 64;

pub(crate) const ROOT: &str = "aseman/capsule/";
const LAYOUT_KEY: &str = "aseman/capsule/layout";
const WRITE_ATTEMPTS: usize = 3;
const MAX_PREDICATE_WIDTH: usize = 64;

const CORE_SCHEMAS: &str =
    include_str!("../../../../contracts/capsule/kinds/core-logical-schemas.json");
const CLASS_SCHEMAS: &str =
    include_str!("../../../../contracts/capsule/kinds/storage-class-logical-schemas.json");

#[derive(Deserialize)]
struct LogicalSchemas {
    definitions: Vec<LogicalSchema>,
}

#[derive(Deserialize)]
struct LogicalSchema {
    kind: String,
    #[serde(default)]
    unique_indexes: Vec<Vec<String>>,
}

static UNIQUE_INDEXES: OnceLock<BTreeMap<String, Vec<Vec<String>>>> = OnceLock::new();

/// Unique indexes per kind, from the provider-neutral logical schemas.
fn unique_indexes(kind: &str) -> &'static [Vec<String>] {
    UNIQUE_INDEXES
        .get_or_init(|| {
            let mut indexes = BTreeMap::new();
            for source in [CORE_SCHEMAS, CLASS_SCHEMAS] {
                // The schemas are compiled in and checked by their own generators and
                // tests; an unreadable one would fail every build that embeds it.
                if let Ok(schemas) = serde_json::from_str::<LogicalSchemas>(source) {
                    for schema in schemas.definitions {
                        indexes.insert(schema.kind, schema.unique_indexes);
                    }
                }
            }
            indexes
        })
        .get(kind)
        .map_or(&[], Vec::as_slice)
}

fn failed(message: impl Into<String>) -> CapsuleStoreError {
    CapsuleStoreError::Failed(message.into())
}

pub(crate) fn invalid(message: impl std::fmt::Display) -> CapsuleStoreError {
    failed(format!("invalid capsule or query: {message}"))
}

fn unsupported(message: impl std::fmt::Display) -> CapsuleStoreError {
    failed(format!("unsupported storage capability: {message}"))
}

pub(crate) fn storage(error: LegacyMigrationError) -> CapsuleStoreError {
    failed(format!("RocksDB is unavailable: {error}"))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            [
                DIGITS[usize::from(byte >> 4)],
                DIGITS[usize::from(byte & 15)],
            ]
        })
        .map(char::from)
        .collect()
}

fn packed_prefix(kind: &str) -> String {
    format!("{ROOT}packed/{kind}/")
}

fn row_prefix(kind: &str) -> String {
    format!("{ROOT}row/{kind}/")
}

fn packed_key(kind: &str, id: &CapsuleId) -> String {
    format!("{}{}", packed_prefix(kind), hex(&id.0))
}

fn capsule_row_prefix(kind: &str, id: &CapsuleId) -> String {
    format!("{}{}/", row_prefix(kind), hex(&id.0))
}

const META: &str = "$";
const WHOLE_BODY: &str = "b";
const FIELD: &str = "f/";

/// How a flattened capsule's body is stored.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BodyForm {
    /// A tombstone.
    Absent,
    /// An object: one key per field.
    Object,
    /// Any other value: one key for the whole body.
    Value,
}

/// The envelope of a flattened capsule, without its body.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RowMeta {
    envelope: CapsuleEnvelope,
    body: BodyForm,
}

/// What one capsule currently occupies: its keys and their values.
#[derive(Default)]
pub(crate) struct Stored {
    keys: BTreeMap<String, Vec<u8>>,
}

impl Stored {
    fn expectations(&self, kind: &str, id: &CapsuleId) -> Vec<KvExpectation> {
        // The packed key and the metadata key change on every write, so holding them
        // fixed pins the whole capsule.
        [
            packed_key(kind, id),
            format!("{}{META}", capsule_row_prefix(kind, id)),
        ]
        .into_iter()
        .map(|key| {
            let value = self.keys.get(&key).map(Vec::as_slice);
            KvExpectation::holds(key.into_bytes(), value)
        })
        .collect()
    }

    pub(crate) fn envelope(&self, kind: &str, id: &CapsuleId) -> CapsuleStoreResult<Option<CapsuleEnvelope>> {
        if let Some(bytes) = self.keys.get(&packed_key(kind, id)) {
            return CapsuleEnvelope::from_canonical_bytes(bytes)
                .map(Some)
                .map_err(invalid);
        }
        let prefix = capsule_row_prefix(kind, id);
        let Some(meta) = self.keys.get(&format!("{prefix}{META}")) else {
            return Ok(None);
        };
        let meta: RowMeta = serde_json::from_slice(meta).map_err(invalid)?;
        let mut envelope = meta.envelope;
        envelope.body = match meta.body {
            BodyForm::Absent => None,
            BodyForm::Value => Some(
                decode_canonical_value(
                    self.keys
                        .get(&format!("{prefix}{WHOLE_BODY}"))
                        .ok_or_else(|| invalid("a stored capsule lost its body"))?,
                )
                .map_err(invalid)?,
            ),
            BodyForm::Object => {
                let field_prefix = format!("{prefix}{FIELD}");
                let mut body = BTreeMap::new();
                for (key, value) in self.keys.range(field_prefix.clone()..) {
                    let Some(name) = key.strip_prefix(&field_prefix) else {
                        break;
                    };
                    body.insert(
                        name.to_owned(),
                        decode_canonical_value(value).map_err(invalid)?,
                    );
                }
                Some(CapsuleValue::Object(body))
            }
        };
        envelope
            .verify()
            .map_err(|error| invalid(format!("stored row does not match its capsule: {error}")))?;
        Ok(Some(envelope))
    }
}

/// The keys `capsule` occupies in `layout`, excluding unique-index keys.
fn capsule_keys(
    capsule: &CapsuleEnvelope,
    layout: CapsuleLayout,
) -> CapsuleStoreResult<BTreeMap<String, Vec<u8>>> {
    let kind = &capsule.kind.0;
    let mut keys = BTreeMap::new();
    match layout {
        CapsuleLayout::Capsule => {
            keys.insert(
                packed_key(kind, &capsule.id),
                capsule.canonical_bytes().map_err(invalid)?,
            );
        }
        CapsuleLayout::Flattened => {
            let prefix = capsule_row_prefix(kind, &capsule.id);
            let body = match &capsule.body {
                None => BodyForm::Absent,
                Some(CapsuleValue::Object(fields)) => {
                    for (name, value) in fields {
                        keys.insert(
                            format!("{prefix}{FIELD}{name}"),
                            encode_value(value).map_err(invalid)?,
                        );
                    }
                    BodyForm::Object
                }
                Some(value) => {
                    keys.insert(
                        format!("{prefix}{WHOLE_BODY}"),
                        encode_value(value).map_err(invalid)?,
                    );
                    BodyForm::Value
                }
            };
            let mut envelope = capsule.clone();
            envelope.body = None;
            keys.insert(
                format!("{prefix}{META}"),
                serde_json::to_vec(&RowMeta { envelope, body }).map_err(invalid)?,
            );
        }
    }
    Ok(keys)
}

/// The unique-index keys a live capsule holds.
fn unique_keys(capsule: &CapsuleEnvelope) -> CapsuleStoreResult<Vec<String>> {
    let Some(CapsuleValue::Object(body)) = &capsule.body else {
        return Ok(Vec::new());
    };
    let mut keys = Vec::new();
    for index in unique_indexes(&capsule.kind.0) {
        let mut values = Vec::with_capacity(index.len());
        for field in index {
            let value = match body.get(field) {
                Some(value) => value.clone(),
                None => capsule
                    .relationships
                    .iter()
                    .find(|relationship| &relationship.name == field)
                    .map_or(CapsuleValue::Null, |relationship| {
                        CapsuleValue::Bytes(relationship.target_id.0.to_vec())
                    }),
            };
            values.push(value);
        }
        // Like a SQL unique index, a missing value never collides.
        if values
            .iter()
            .any(|value| matches!(value, CapsuleValue::Null))
        {
            continue;
        }
        keys.push(format!(
            "{ROOT}unique/{}/{}/{}",
            capsule.kind.0,
            index.join(","),
            hex(&encode_value(&CapsuleValue::Array(values)).map_err(invalid)?)
        ));
    }
    Ok(keys)
}

/// Capsules on the provider's key/value store, in either layout.
pub struct RocksDbCapsuleStore {
    pub(crate) kv: Arc<dyn LegacyKvStore>,
    capsule: AtomicBool,
    replicated: bool,
}

impl RocksDbCapsuleStore {
    /// A store over `kv` that writes in the layout it was last migrated to.
    /// `replicated` says whether `kv` is a Raft-replicated cluster store.
    pub fn open(kv: Arc<dyn LegacyKvStore>, replicated: bool) -> CapsuleStoreResult<Self> {
        let layout = match kv.get(LAYOUT_KEY.as_bytes()).map_err(storage)?.as_deref() {
            Some(b"capsule") => CapsuleLayout::Capsule,
            _ => CapsuleLayout::Flattened,
        };
        Ok(Self {
            kv,
            capsule: AtomicBool::new(layout == CapsuleLayout::Capsule),
            replicated,
        })
    }

    #[must_use]
    pub fn layout(&self) -> CapsuleLayout {
        if self.capsule.load(AtomicOrdering::Acquire) {
            CapsuleLayout::Capsule
        } else {
            CapsuleLayout::Flattened
        }
    }

    #[must_use]
    pub fn capabilities(&self) -> ProviderCapabilities {
        let mut capabilities = BTreeSet::from([
            StorageCapability::TransactionsSingleCapsule,
            StorageCapability::TransactionsMultiCapsule,
            StorageCapability::QueriesRange,
            StorageCapability::IndexesUnique,
        ]);
        // Writes are linearized by the Raft log, but a replica serves reads from its
        // own applied state, which may trail the leader.
        capabilities.insert(if self.replicated {
            StorageCapability::ConsistencyReadCommitted
        } else {
            StorageCapability::ConsistencyLinearizable
        });
        ProviderCapabilities {
            provider_id: "rocksdb-capsule-v1".to_owned(),
            capabilities,
            max_query_limit: MAX_QUERY_LIMIT,
            max_transaction_capsules: MAX_TRANSACTION_CAPSULES as u32,
        }
    }

    /// Record `layout` and rewrite every capsule stored in the other layout. Returns
    /// how many capsules were rewritten.
    pub fn migrate_layout(&self, layout: CapsuleLayout) -> CapsuleStoreResult<u64> {
        let name: &[u8] = match layout {
            CapsuleLayout::Flattened => b"flattened",
            CapsuleLayout::Capsule => b"capsule",
        };
        self.kv
            .write_batch(&[LegacyKvWrite::Put {
                key: LAYOUT_KEY.as_bytes().to_vec(),
                value: name.to_vec(),
            }])
            .map_err(storage)?;
        self.capsule
            .store(layout == CapsuleLayout::Capsule, AtomicOrdering::Release);
        let source = match layout {
            CapsuleLayout::Flattened => format!("{ROOT}packed/"),
            CapsuleLayout::Capsule => format!("{ROOT}row/"),
        };
        let mut identities = BTreeSet::new();
        for (key, _) in self.kv.scan_prefix(source.as_bytes()).map_err(storage)? {
            let key = String::from_utf8_lossy(&key).into_owned();
            let mut parts = key[source.len()..].split('/');
            if let (Some(kind), Some(id)) = (parts.next(), parts.next())
                && let Some(id) = parse_id(id)
            {
                identities.insert((kind.to_owned(), id));
            }
        }
        let mut converted = 0;
        for (kind, id) in identities {
            for attempt in 1..=WRITE_ATTEMPTS {
                let stored = self.stored(&kind, &id)?;
                let Some(envelope) = stored.envelope(&kind, &id)? else {
                    break;
                };
                let target = capsule_keys(&envelope, layout)?;
                let writes = replace(&stored.keys, &target);
                if self
                    .kv
                    .write_batch_if(&stored.expectations(&kind, &id), &writes)
                    .map_err(storage)?
                {
                    converted += 1;
                    break;
                }
                if attempt == WRITE_ATTEMPTS {
                    return Err(CapsuleStoreError::Conflict);
                }
            }
        }
        Ok(converted)
    }

    pub(crate) fn stored(&self, kind: &str, id: &CapsuleId) -> CapsuleStoreResult<Stored> {
        let mut stored = Stored::default();
        let packed = packed_key(kind, id);
        if let Some(value) = self.kv.get(packed.as_bytes()).map_err(storage)? {
            stored.keys.insert(packed, value);
        }
        for (key, value) in self
            .kv
            .scan_prefix(capsule_row_prefix(kind, id).as_bytes())
            .map_err(storage)?
        {
            stored
                .keys
                .insert(String::from_utf8(key).map_err(invalid)?, value);
        }
        Ok(stored)
    }

    pub(crate) fn try_put_all(
        &self,
        writes: &[(CapsuleEnvelope, Option<u64>)],
    ) -> CapsuleStoreResult<bool> {
        self.try_write(writes, false)
    }

    /// Store capsules exactly as given (migration import): a capsule of any revision
    /// is inserted when its id is free, an identical one is accepted, anything else is
    /// a conflict.
    pub(crate) fn try_import(&self, capsules: &[CapsuleEnvelope]) -> CapsuleStoreResult<bool> {
        let writes = capsules
            .iter()
            .map(|capsule| (capsule.clone(), None))
            .collect::<Vec<_>>();
        self.try_write(&writes, true)
    }

    fn try_write(
        &self,
        writes: &[(CapsuleEnvelope, Option<u64>)],
        import: bool,
    ) -> CapsuleStoreResult<bool> {
        let layout = self.layout();
        // Each capsule's state before this batch, and after the writes so far.
        let mut before: BTreeMap<(String, CapsuleId), Stored> = BTreeMap::new();
        let mut after: BTreeMap<(String, CapsuleId), Option<CapsuleEnvelope>> = BTreeMap::new();
        for (capsule, expected_revision) in writes {
            capsule.verify().map_err(invalid)?;
            let identity = (capsule.kind.0.clone(), capsule.id.clone());
            if !before.contains_key(&identity) {
                let stored = self.stored(&identity.0, &identity.1)?;
                after.insert(identity.clone(), stored.envelope(&identity.0, &identity.1)?);
                before.insert(identity.clone(), stored);
            }
            let current = after[&identity].as_ref();
            let accepted = match (expected_revision, current) {
                (None, None) => import || capsule.revision == 1,
                (Some(expected), Some(current)) => {
                    current.revision == *expected
                        && capsule.revision == expected.saturating_add(1)
                        && capsule.created_at_micros == current.created_at_micros
                        && capsule.previous_integrity.as_ref() == Some(&current.integrity_hash)
                }
                _ => false,
            };
            if !accepted {
                // An identical replay of the stored revision succeeds.
                if current.is_some_and(|current| current == capsule) {
                    continue;
                }
                return Err(CapsuleStoreError::Conflict);
            }
            after.insert(identity, Some(capsule.clone()));
        }

        let mut expectations = Vec::new();
        let mut batch = Vec::new();
        // Unique keys this batch releases and claims, with the capsule doing it.
        let mut released: BTreeMap<String, CapsuleId> = BTreeMap::new();
        let mut claimed: BTreeMap<String, CapsuleId> = BTreeMap::new();
        for ((kind, id), stored) in &before {
            let Some(Some(target)) = after.get(&(kind.clone(), id.clone())) else {
                continue;
            };
            let previous = stored.envelope(kind, id)?;
            if previous.as_ref() == Some(target) {
                continue;
            }
            expectations.extend(stored.expectations(kind, id));
            batch.extend(replace(&stored.keys, &capsule_keys(target, layout)?));
            if let Some(previous) = &previous {
                for key in unique_keys(previous)? {
                    released.insert(key, id.clone());
                }
            }
            // Secondary indexes (ADR 0036) follow the live revision.
            let old_index = match &previous {
                Some(previous) => crate::model_store::index_keys(previous)?,
                None => BTreeSet::new(),
            };
            let new_index = crate::model_store::index_keys(target)?;
            for key in old_index.difference(&new_index) {
                batch.push(LegacyKvWrite::Delete {
                    key: key.as_bytes().to_vec(),
                });
            }
            for key in new_index.difference(&old_index) {
                batch.push(LegacyKvWrite::Put {
                    key: key.as_bytes().to_vec(),
                    value: Vec::new(),
                });
            }
            if !target.tombstone {
                for key in unique_keys(target)? {
                    if claimed.get(&key).is_some_and(|owner| owner != id) {
                        return Err(CapsuleStoreError::Conflict);
                    }
                    claimed.insert(key, id.clone());
                }
            }
        }
        for (key, id) in &claimed {
            let owner = self.kv.get(key.as_bytes()).map_err(storage)?;
            if let Some(owner) = &owner
                && owner.as_slice() != id.0
                && released
                    .get(key)
                    .is_none_or(|releaser| releaser.0 != owner.as_slice())
            {
                return Err(CapsuleStoreError::Conflict);
            }
            expectations.push(KvExpectation::holds(
                key.as_bytes().to_vec(),
                owner.as_deref(),
            ));
        }
        for key in released.keys() {
            if !claimed.contains_key(key) {
                batch.push(LegacyKvWrite::Delete {
                    key: key.as_bytes().to_vec(),
                });
            }
        }
        for (key, id) in claimed {
            batch.push(LegacyKvWrite::Put {
                key: key.into_bytes(),
                value: id.0.to_vec(),
            });
        }
        if batch.is_empty() {
            return Ok(true);
        }
        self.kv
            .write_batch_if(&expectations, &batch)
            .map_err(storage)
    }

    pub(crate) fn scan_kind(&self, kind: &str) -> CapsuleStoreResult<Vec<CapsuleEnvelope>> {
        let mut capsules = Vec::new();
        for (_, value) in self
            .kv
            .scan_prefix(packed_prefix(kind).as_bytes())
            .map_err(storage)?
        {
            capsules.push(CapsuleEnvelope::from_canonical_bytes(&value).map_err(invalid)?);
        }
        let prefix = row_prefix(kind);
        let mut rows: BTreeMap<CapsuleId, Stored> = BTreeMap::new();
        for (key, value) in self.kv.scan_prefix(prefix.as_bytes()).map_err(storage)? {
            let key = String::from_utf8(key).map_err(invalid)?;
            let Some(id) = key[prefix.len()..].split('/').next().and_then(parse_id) else {
                return Err(invalid("a stored capsule key is malformed"));
            };
            rows.entry(id).or_default().keys.insert(key, value);
        }
        for (id, stored) in rows {
            if let Some(capsule) = stored.envelope(kind, &id)? {
                capsules.push(capsule);
            }
        }
        Ok(capsules)
    }
}

/// Writes that turn the keys in `current` into exactly the keys in `target`.
fn replace(
    current: &BTreeMap<String, Vec<u8>>,
    target: &BTreeMap<String, Vec<u8>>,
) -> Vec<LegacyKvWrite> {
    let mut writes = current
        .keys()
        .filter(|key| !target.contains_key(*key))
        .map(|key| LegacyKvWrite::Delete {
            key: key.as_bytes().to_vec(),
        })
        .collect::<Vec<_>>();
    writes.extend(
        target
            .iter()
            .filter(|(key, value)| current.get(*key) != Some(*value))
            .map(|(key, value)| LegacyKvWrite::Put {
                key: key.as_bytes().to_vec(),
                value: value.clone(),
            }),
    );
    writes
}

pub(crate) fn parse_id(text: &str) -> Option<CapsuleId> {
    if text.len() != 32 {
        return None;
    }
    let mut id = [0_u8; 16];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(CapsuleId(id))
}

impl CapsuleStore for RocksDbCapsuleStore {
    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> CapsuleStoreResult<Option<CapsuleEnvelope>> {
        self.stored(&kind.0, id)?.envelope(&kind.0, id)
    }

    fn put(
        &self,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> CapsuleStoreResult<()> {
        self.put_all(&[(capsule.clone(), expected_revision)])
    }

    fn put_all(&self, writes: &[(CapsuleEnvelope, Option<u64>)]) -> CapsuleStoreResult<()> {
        if writes.len() > MAX_TRANSACTION_CAPSULES {
            return Err(unsupported(format!(
                "a transaction holds at most {MAX_TRANSACTION_CAPSULES} capsules"
            )));
        }
        // A lost race re-reads and decides again: an identical replay then succeeds and
        // a real conflict is reported.
        for _ in 0..WRITE_ATTEMPTS {
            if self.try_put_all(writes)? {
                return Ok(());
            }
        }
        Err(CapsuleStoreError::Conflict)
    }

    fn query(&self, query: &CapsuleQuery) -> CapsuleStoreResult<Vec<CapsuleEnvelope>> {
        if query.limit == 0 || query.limit > MAX_QUERY_LIMIT {
            return Err(invalid("query limit is outside provider bounds"));
        }
        if !query.aggregates.is_empty() || !query.traversals.is_empty() || query.cursor.is_some() {
            return Err(unsupported(
                "aggregates, traversal, and cursors are not advertised by rocksdb-capsule-v1",
            ));
        }
        if let Some(predicate) = &query.predicate {
            check_predicate(predicate, 1)?;
        }
        let mut rows = Vec::new();
        for capsule in self.scan_kind(&query.kind.0)? {
            if capsule.tombstone {
                continue;
            }
            let keep = match &query.predicate {
                Some(predicate) => evaluate(predicate, &capsule) == Some(true),
                None => true,
            };
            if keep {
                rows.push(capsule);
            }
        }
        rows.sort_by(|left, right| {
            for sort in &query.sort {
                let ordering = sort_order(
                    &field(left, &sort.field),
                    &field(right, &sort.field),
                    &sort.direction,
                );
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            left.id.cmp(&right.id)
        });
        rows.truncate(query.limit as usize);
        Ok(rows)
    }
}

fn check_predicate(predicate: &QueryPredicate, depth: usize) -> CapsuleStoreResult<()> {
    if depth > MAX_QUERY_DEPTH {
        return Err(invalid("query predicate nesting exceeds the limit"));
    }
    match predicate {
        QueryPredicate::Compare {
            operator, value, ..
        } => {
            if matches!(value, CapsuleValue::Null)
                && !matches!(
                    operator,
                    ComparisonOperator::Equal | ComparisonOperator::NotEqual
                )
            {
                return Err(invalid("null supports only equality comparisons"));
            }
            Ok(())
        }
        QueryPredicate::And { predicates } | QueryPredicate::Or { predicates } => {
            if predicates.is_empty() || predicates.len() > MAX_PREDICATE_WIDTH {
                return Err(invalid("boolean predicate width is invalid"));
            }
            predicates
                .iter()
                .try_for_each(|child| check_predicate(child, depth + 1))
        }
        QueryPredicate::Not { predicate } => check_predicate(predicate, depth + 1),
        QueryPredicate::RelationshipExists { .. } => Ok(()),
    }
}

/// A body field, or a relationship's target id, of `capsule` (`Null` when absent).
fn field(capsule: &CapsuleEnvelope, name: &str) -> CapsuleValue {
    if let Some(CapsuleValue::Object(body)) = &capsule.body
        && let Some(value) = body.get(name)
    {
        return value.clone();
    }
    capsule
        .relationships
        .iter()
        .find(|relationship| relationship.name == name)
        .map_or(CapsuleValue::Null, |relationship| {
            CapsuleValue::Bytes(relationship.target_id.0.to_vec())
        })
}

/// SQL's three-valued comparison: `None` is unknown (a `NULL` or incomparable operand).
fn evaluate(predicate: &QueryPredicate, capsule: &CapsuleEnvelope) -> Option<bool> {
    match predicate {
        QueryPredicate::Compare {
            field: name,
            operator,
            value,
        } => {
            let actual = field(capsule, name);
            if matches!(value, CapsuleValue::Null) {
                let is_null = matches!(actual, CapsuleValue::Null);
                return Some(match operator {
                    ComparisonOperator::NotEqual => !is_null,
                    _ => is_null,
                });
            }
            let ordering = compare(&actual, value)?;
            Some(match operator {
                ComparisonOperator::Equal => ordering == Ordering::Equal,
                ComparisonOperator::NotEqual => ordering != Ordering::Equal,
                ComparisonOperator::LessThan => ordering == Ordering::Less,
                ComparisonOperator::LessOrEqual => ordering != Ordering::Greater,
                ComparisonOperator::GreaterThan => ordering == Ordering::Greater,
                ComparisonOperator::GreaterOrEqual => ordering != Ordering::Less,
            })
        }
        QueryPredicate::And { predicates } => {
            let mut result = Some(true);
            for child in predicates {
                match evaluate(child, capsule) {
                    Some(false) => return Some(false),
                    None => result = None,
                    Some(true) => {}
                }
            }
            result
        }
        QueryPredicate::Or { predicates } => {
            let mut result = Some(false);
            for child in predicates {
                match evaluate(child, capsule) {
                    Some(true) => return Some(true),
                    None => result = None,
                    Some(false) => {}
                }
            }
            result
        }
        QueryPredicate::Not { predicate } => evaluate(predicate, capsule).map(|value| !value),
        QueryPredicate::RelationshipExists { relationship } => Some(
            capsule
                .relationships
                .iter()
                .any(|candidate| &candidate.name == relationship),
        ),
    }
}

/// Order two scalar values of comparable types; `None` for `NULL` or a type mismatch.
fn compare(left: &CapsuleValue, right: &CapsuleValue) -> Option<Ordering> {
    use CapsuleValue::{Bool, Bytes, Float, Integer, Text};
    match (left, right) {
        (Bool(left), Bool(right)) => Some(left.cmp(right)),
        (Integer(left), Integer(right)) => Some(left.cmp(right)),
        (Float(left), Float(right)) => left.partial_cmp(right),
        (Integer(left), Float(right)) => (*left as f64).partial_cmp(right),
        (Float(left), Integer(right)) => left.partial_cmp(&(*right as f64)),
        (Text(left), Text(right)) => Some(left.cmp(right)),
        (Bytes(left), Bytes(right)) => Some(left.cmp(right)),
        _ => None,
    }
}

/// PostgreSQL's default order: `NULL` last ascending and first descending.
fn sort_order(left: &CapsuleValue, right: &CapsuleValue, direction: &SortDirection) -> Ordering {
    let ascending = match (left, right) {
        (CapsuleValue::Null, CapsuleValue::Null) => Ordering::Equal,
        (CapsuleValue::Null, _) => Ordering::Greater,
        (_, CapsuleValue::Null) => Ordering::Less,
        (left, right) => compare(left, right).unwrap_or(Ordering::Equal),
    };
    match direction {
        SortDirection::Ascending => ascending,
        SortDirection::Descending => ascending.reverse(),
    }
}

#[cfg(test)]
mod tests;
