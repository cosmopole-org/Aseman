//! Storage migration (ADR 0036): what `asemanctl storage migrate` (and the node's
//! `storage migrate` for a containerized deployment) runs while the node is stopped.
//!
//! - **Legacy conversion.** A store from before ADR 0036 — the RocksDB key/value base
//!   or PostgreSQL's `aseman_compat` schema — is read as legacy physical records,
//!   transformed by the reviewed A309 transform, and written as models: capsules
//!   import verbatim (a keyed model gains its natural `key`), guest pairs become
//!   `core.guest_pair` rows, the legacy finance epoch is kept (ADR 0017) and bridged
//!   into the live finance models, and the id counters carry over. Consensus logs the
//!   engine kept under absolute directories move to their relative names.
//! - **Provider copy.** Every model's capsules (tombstones and revision chains
//!   included) and every consensus log copy into an empty target provider, and are
//!   verified by count and digest.
//!
//! A converted legacy layout is retired (renamed aside, kept for inspection) only
//! when the conversion happened in place; a copy leaves the source untouched as the
//! rollback.

use crate::{SecretOverrides, provider_name, registry, settings};
use aseman_capsule::finance::StorageFinanceLedger;
use aseman_capsule::guest_kv;
use aseman_config::AsemanConfig;
use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleValue, OwnerScope};
use aseman_domain::{CreatureId, Uuid};
use aseman_ports::consensus_log::{ConsensusLogStorage, ConsensusLogWrite};
use aseman_ports::finance_ledger::{FinanceDoc, FinanceLedger, WalletCounter};
use aseman_storage::client::core::{counter, finance_journal_participant, marker};
use aseman_storage::provider::StorageProvider;
use aseman_storage::schema::{Model, Schema};
use aseman_storage::{Id, Mode, Models, ProviderSettings, Storage, StorageError, StorageResult};
use aseman_storage_rocksdb::consensus_log::RocksDbConsensusLogStorage;
use aseman_storage_rocksdb::model_store::{LEGACY_LOG_DIRECTORY, legacy_consensus_logs};
use aseman_storage_rocksdb::{
    DEFAULT_MAX_EXPORT_BYTES, DEFAULT_MAX_EXPORT_RECORDS, LEGACY_GUEST_KV_KIND,
    LegacyFileArtifactEvidence, LegacyFinanceConfig, LegacyPathArtifact, LegacyPhysicalRecord,
    LegacyRecordSource, LegacySecretMasterKey, LegacySignalLogSource, LegacySignalStreamPolicy,
    LegacySnapshotGraph, LegacyTransformEvidence, RocksDbLegacySource, read_legacy_signals,
    transform_legacy_signal_rows,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Capsules one export page carries.
const PAGE: usize = 1_000;
/// Entries one consensus-log write carries.
const LOG_BATCH: usize = 1_000;
/// The legacy node secret key file under the storage root (ADR 0023).
const SECRET_KEY_FILE: &str = "node-secret-key";
/// Where artifacts found outside the storage root are copied (ADR 0022).
const MIGRATED_ARTIFACTS: &str = "migrated-artifacts";

fn invalid(message: impl Into<String>) -> StorageError {
    StorageError::Invalid(message.into())
}

fn legacy(error: impl std::fmt::Display) -> StorageError {
    StorageError::Invalid(format!("legacy store: {error}"))
}

fn port(error: impl std::fmt::Display) -> StorageError {
    StorageError::Unavailable(error.to_string())
}

/// What a migration did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Report {
    /// Capsules written per model.
    pub models: BTreeMap<String, u64>,
    /// Entries written per consensus log.
    pub consensus_logs: BTreeMap<String, u64>,
    /// Guest pairs converted into `core.guest_pair`.
    pub guest_pairs: u64,
    /// Finance records bridged into the live finance models.
    pub finance_records: u64,
    /// What the operator should know.
    pub notes: Vec<String>,
}

impl std::fmt::Display for Report {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (model, count) in self.models.iter().filter(|(_, count)| **count > 0) {
            writeln!(formatter, "  {model}: {count} record(s)")?;
        }
        for (log, count) in &self.consensus_logs {
            writeln!(formatter, "  consensus log {log}: {count} entr(ies)")?;
        }
        if self.guest_pairs > 0 {
            writeln!(formatter, "  guest pairs: {}", self.guest_pairs)?;
        }
        if self.finance_records > 0 {
            writeln!(
                formatter,
                "  finance records bridged: {}",
                self.finance_records
            )?;
        }
        for note in &self.notes {
            writeln!(formatter, "  note: {note}")?;
        }
        Ok(())
    }
}

// ───────────────────────────── models ─────────────────────────────

/// Models in an order where every relation's target comes before its referrer
/// (PostgreSQL enforces relations as foreign keys).
fn dependency_order(schema: &Schema) -> Vec<&Model> {
    fn visit<'a>(
        model: &'a Model,
        schema: &'a Schema,
        seen: &mut BTreeSet<String>,
        order: &mut Vec<&'a Model>,
    ) {
        if !seen.insert(model.name.clone()) {
            return;
        }
        for target in model.relations.values() {
            if let Ok(target) = schema.model(target) {
                visit(target, schema, seen, order);
            }
        }
        order.push(model);
    }
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    let mut models = schema.models().collect::<Vec<_>>();
    models.sort_by(|left, right| left.name.cmp(&right.name));
    for model in models {
        visit(model, schema, &mut seen, &mut order);
    }
    order
}

/// Every capsule of `model` (tombstones included) and their digest.
fn inventory(provider: &dyn StorageProvider, model: &Model) -> StorageResult<(u64, [u8; 32])> {
    let mut hasher = Sha256::new();
    let mut count = 0;
    let mut after = None;
    loop {
        let page = provider.export(model, after, PAGE)?;
        let Some(last) = page.last() else {
            break;
        };
        after = Some(Id(last.id.0));
        for capsule in &page {
            hasher.update(capsule.id.0);
            hasher.update(capsule.revision.to_be_bytes());
            hasher.update(&capsule.integrity_hash.bytes);
        }
        count += page.len() as u64;
        if page.len() < PAGE {
            break;
        }
    }
    Ok((count, hasher.finalize().into()))
}

/// Refuse a target that already holds records or consensus logs.
///
/// # Errors
///
/// A non-empty target, or a provider failure.
pub fn ensure_empty(target: &Storage) -> StorageResult<()> {
    for model in target.schema().models() {
        if !target.provider().export(model, None, 1)?.is_empty() {
            return Err(invalid(format!(
                "the target already holds {} records; migrate into an empty store",
                model.name
            )));
        }
    }
    if let Some(log) = target
        .provider()
        .consensus_logs()
        .names()
        .map_err(port)?
        .first()
    {
        return Err(invalid(format!(
            "the target already holds consensus log {log}; migrate into an empty store"
        )));
    }
    Ok(())
}

/// Copy every model's capsules from `source` into `target` and verify them.
///
/// # Errors
///
/// A provider failure, or a copy that does not verify.
pub fn copy_models(source: &Storage, target: &Storage, report: &mut Report) -> StorageResult<()> {
    for model in dependency_order(source.schema()) {
        let mut after = None;
        let mut copied = 0;
        loop {
            let page = source.provider().export(model, after, PAGE)?;
            let Some(last) = page.last() else {
                break;
            };
            after = Some(Id(last.id.0));
            target.provider().import(model, &page)?;
            copied += page.len() as u64;
            if page.len() < PAGE {
                break;
            }
        }
        if copied == 0 {
            continue;
        }
        if inventory(source.provider().as_ref(), model)?
            != inventory(target.provider().as_ref(), model)?
        {
            return Err(invalid(format!("{} did not copy exactly", model.name)));
        }
        *report.models.entry(model.name.clone()).or_default() += copied;
    }
    Ok(())
}

// ───────────────────────────── consensus logs ─────────────────────────────

/// Copy the log `from` of `source` into the log `to` of `target`; the entry count.
fn copy_log(
    source: &dyn ConsensusLogStorage,
    from: &str,
    target: &dyn ConsensusLogStorage,
    to: &str,
) -> StorageResult<u64> {
    let entries = source
        .open(from, false)
        .map_err(port)?
        .scan_prefix(b"")
        .map_err(port)?;
    let log = target.open(to, false).map_err(port)?;
    for batch in entries.chunks(LOG_BATCH) {
        let writes = batch
            .iter()
            .map(|(key, value)| ConsensusLogWrite::Put {
                key: key.clone(),
                value: value.clone(),
            })
            .collect::<Vec<_>>();
        log.write(&writes).map_err(port)?;
    }
    log.flush().map_err(port)?;
    if log.scan_prefix(b"").map_err(port)? != entries {
        return Err(invalid(format!(
            "consensus log {from} did not copy exactly"
        )));
    }
    Ok(entries.len() as u64)
}

/// Copy every consensus log of `source` into `target` under its own name.
///
/// # Errors
///
/// A provider failure, or a copy that does not verify.
pub fn copy_consensus_logs(
    source: &dyn ConsensusLogStorage,
    target: &dyn ConsensusLogStorage,
    report: &mut Report,
) -> StorageResult<()> {
    for name in source.names().map_err(port)? {
        if name.starts_with('/') {
            // An absolute legacy name moves with the legacy conversion.
            continue;
        }
        let count = copy_log(source, &name, target, &name)?;
        report.consensus_logs.insert(name, count);
    }
    Ok(())
}

/// The relative name of a log the engine named by its absolute directory.
fn relative_log_name(absolute: &str, storage_root: &Path) -> String {
    let path = Path::new(absolute);
    let path = path.strip_prefix(storage_root).unwrap_or(path);
    let path = path.strip_prefix("/").unwrap_or(path);
    let path = if path
        .file_name()
        .is_some_and(|name| name == LEGACY_LOG_DIRECTORY)
    {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    path.to_string_lossy().into_owned()
}

/// Move the consensus logs kept before ADR 0036 into `target` under their relative
/// names: the RocksDB provider's per-chain directories under the storage root, or a
/// PostgreSQL log named by an absolute directory (set aside once copied).
fn relocate_legacy_logs(
    provider: &str,
    source_logs: &dyn ConsensusLogStorage,
    storage_root: &Path,
    target: &dyn ConsensusLogStorage,
    tuning: aseman_config::RocksDbTuning,
    report: &mut Report,
) -> StorageResult<()> {
    if provider == aseman_storage_rocksdb::model_store::NAME {
        let root = RocksDbConsensusLogStorage::new(storage_root, tuning);
        for log in legacy_consensus_logs(storage_root).map_err(port)? {
            let from = log
                .path
                .strip_prefix(storage_root)
                .map_err(port)?
                .to_string_lossy()
                .into_owned();
            let count = copy_log(&root, &from, target, &log.name)?;
            report.consensus_logs.insert(log.name, count);
        }
        return Ok(());
    }
    for name in source_logs.names().map_err(port)? {
        if !name.starts_with('/') {
            continue;
        }
        let relative = relative_log_name(&name, storage_root);
        let count = copy_log(source_logs, &name, target, &relative)?;
        report.consensus_logs.insert(relative, count);
    }
    Ok(())
}

// ───────────────────────────── legacy conversion ─────────────────────────────

/// Where a legacy store's records are read from.
pub enum LegacySource {
    /// A RocksDB key/value base directory.
    RocksDb(PathBuf),
    /// A PostgreSQL database with the `aseman_compat` schema.
    Postgres(String),
}

/// What the legacy transform needs besides the records.
#[derive(Clone, Debug, Default)]
pub struct LegacyOptions {
    pub storage_root: PathBuf,
    /// The legacy finance epoch's currency and scale.
    pub currency: String,
    pub scale: u8,
    /// Legacy id origins this installation owns (`global` and the node's own).
    pub local_origins: BTreeSet<String>,
    /// Where each legacy `File` object's bytes are (`--file-artifact ID=PATH`).
    pub file_artifacts: BTreeMap<String, PathBuf>,
    /// Where the legacy signal history is (`None`: it is not migrated).
    pub signal_log: Option<LegacySignalLogSource>,
}

/// The legacy physical records of `source`.
fn legacy_records(source: &LegacySource) -> StorageResult<Vec<LegacyPhysicalRecord>> {
    match source {
        LegacySource::RocksDb(path) => {
            let snapshot = RocksDbLegacySource::open_read_only(path, "asemanctl-storage-migrate")
                .map_err(legacy)?
                .read_snapshot(DEFAULT_MAX_EXPORT_RECORDS, DEFAULT_MAX_EXPORT_BYTES)
                .map_err(legacy)?;
            Ok(snapshot.records)
        }
        LegacySource::Postgres(url) => {
            Ok(aseman_storage_postgres::compatibility::legacy_records(url)
                .map_err(legacy)?
                .into_iter()
                .map(|(key, value)| LegacyPhysicalRecord {
                    family: "application-postgres-compat".to_owned(),
                    key: key.into_bytes(),
                    value,
                })
                .collect())
        }
    }
}

fn artifact_evidence(
    storage_root: &Path,
    path: &Path,
) -> StorageResult<LegacyFileArtifactEvidence> {
    let bytes = std::fs::read(path).map_err(port)?;
    // Blobs are files under the storage root, exactly where legacy kept them; an
    // artifact kept elsewhere is copied in, content-addressed.
    let key = match path.strip_prefix(storage_root) {
        Ok(relative) => relative.to_string_lossy().into_owned(),
        Err(_) => {
            let key = format!(
                "{MIGRATED_ARTIFACTS}/{}",
                hex::encode(Sha256::digest(&bytes))
            );
            let copy = storage_root.join(&key);
            if let Some(parent) = copy.parent() {
                std::fs::create_dir_all(parent).map_err(port)?;
            }
            std::fs::write(&copy, &bytes).map_err(port)?;
            key
        }
    };
    LegacyFileArtifactEvidence::from_bytes(&key, "application/octet-stream", &bytes).map_err(legacy)
}

/// The evidence the transform needs, gathered from the storage root.
fn evidence(
    graph: &LegacySnapshotGraph,
    options: &LegacyOptions,
) -> StorageResult<LegacyTransformEvidence> {
    let needs = graph.artifact_needs().map_err(legacy)?;
    let mut evidence = LegacyTransformEvidence {
        finance: Some(LegacyFinanceConfig {
            currency: options.currency.clone(),
            scale: options.scale,
        }),
        local_origins: options.local_origins.clone(),
        ..LegacyTransformEvidence::default()
    };
    let key_file = options.storage_root.join(SECRET_KEY_FILE);
    if key_file.is_file() {
        let contents = std::fs::read_to_string(&key_file).map_err(port)?;
        evidence.secret_master_key =
            Some(LegacySecretMasterKey::from_key_file(&contents).map_err(legacy)?);
    }
    for path in &needs.paths {
        let artifact = if Path::new(path).is_file() {
            LegacyPathArtifact::Present(artifact_evidence(&options.storage_root, Path::new(path))?)
        } else {
            // Legacy deletes removed files and left their records.
            LegacyPathArtifact::AttestedAbsent
        };
        evidence.path_artifacts.insert(path.clone(), artifact);
    }
    let missing = needs
        .files
        .keys()
        .filter(|id| !options.file_artifacts.contains_key(*id))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(invalid(format!(
            "legacy File objects need their bytes: pass --file-artifact ID=PATH for {}",
            missing.join(", ")
        )));
    }
    for (id, path) in &options.file_artifacts {
        evidence
            .file_artifacts
            .insert(id.clone(), artifact_evidence(&options.storage_root, path)?);
    }
    Ok(evidence)
}

fn text(body: &BTreeMap<String, CapsuleValue>, field: &str) -> Option<String> {
    match body.get(field) {
        Some(CapsuleValue::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

fn body(capsule: &CapsuleEnvelope) -> Option<&BTreeMap<String, CapsuleValue>> {
    match &capsule.body {
        Some(CapsuleValue::Object(fields)) => Some(fields),
        _ => None,
    }
}

/// Give every capsule of a keyed model its natural `key` (the legacy id its
/// `core.legacy_identity` names), resealed.
fn with_keys(
    schema: &Schema,
    capsules: Vec<CapsuleEnvelope>,
) -> StorageResult<Vec<CapsuleEnvelope>> {
    let mut legacy_ids = BTreeMap::new();
    for capsule in &capsules {
        if capsule.kind.0 != "core.legacy_identity" {
            continue;
        }
        if let Some(fields) = body(capsule)
            && let (Some(legacy_id), Some(CapsuleValue::Bytes(target))) =
                (text(fields, "legacy_id"), fields.get("target_id"))
            && let Ok(target) = <[u8; 16]>::try_from(target.as_slice())
        {
            legacy_ids.insert(target, legacy_id);
        }
    }
    capsules
        .into_iter()
        .map(|mut capsule| {
            let keyed = schema
                .model(&capsule.kind.0)
                .ok()
                .is_some_and(|model| model.key_family.is_some());
            if !keyed || capsule.tombstone {
                return Ok(capsule);
            }
            let Some(CapsuleValue::Object(fields)) = &mut capsule.body else {
                return Ok(capsule);
            };
            if fields.contains_key("key") {
                return Ok(capsule);
            }
            let key = legacy_ids.get(&capsule.id.0).ok_or_else(|| {
                invalid(format!(
                    "{} {} has no legacy identity",
                    capsule.kind.0,
                    Id(capsule.id.0)
                ))
            })?;
            fields.insert("key".to_owned(), CapsuleValue::Text(key.clone()));
            capsule.seal().map_err(legacy)
        })
        .collect()
}

/// A legacy guest pair's creature, namespace, name, and value.
fn guest_pair(capsule: &CapsuleEnvelope) -> StorageResult<(CreatureId, String, String, String)> {
    let OwnerScope::Creature(creature) = capsule.owner_scope else {
        return Err(invalid("a legacy guest pair has no creature"));
    };
    let fields = body(capsule).ok_or_else(|| invalid("a legacy guest pair has no body"))?;
    let field = |name: &str| {
        text(fields, name).ok_or_else(|| invalid(format!("a legacy guest pair has no {name}")))
    };
    Ok((
        CreatureId::from_uuid(Uuid::from_bytes(creature)),
        field("namespace")?,
        field("key")?,
        field("value")?,
    ))
}

/// A legacy `link::{family}::{rest}` record, as `(family, rest, text)`.
fn link(record: &LegacyPhysicalRecord) -> Option<(&str, &str, String)> {
    let key = std::str::from_utf8(&record.key)
        .ok()?
        .strip_prefix("link::")?;
    let (family, rest) = key.split_once("::")?;
    Some((family, rest, String::from_utf8(record.value.clone()).ok()?))
}

const MARKER_FAMILIES: [&str; 13] = [
    "FinanceHoldRequest",
    "FinanceRun",
    "FinanceSettlement",
    "FinanceRelease",
    "FinancePayoutRequest",
    "FinancePayoutResolution",
    "FinancePoolOpen",
    "FinancePoolRefresh",
    "FinancePoolClose",
    "FinancePoolSettlement",
    "FinancePoolDebit",
    "PaymentAdjustment",
    "MintApplied",
];

fn wallet_counter(family: &str) -> Option<WalletCounter> {
    Some(match family {
        "FinanceHeld" => WalletCounter::Held,
        "FinanceDebt" => WalletCounter::Debt,
        "FinanceWithdrawable" => WalletCounter::Withdrawable,
        "FinancePayoutHeld" => WalletCounter::PayoutHeld,
        "FinanceEarned" => WalletCounter::Earned,
        "FinanceSpent" => WalletCounter::Spent,
        _ => return None,
    })
}

/// Bridge the imported legacy finance epoch and the legacy finance links into the
/// live finance models (ADR 0017: the epoch itself stays as imported).
fn bridge_finance(
    trx: &aseman_storage::Trx,
    records: &[LegacyPhysicalRecord],
    report: &mut Report,
) -> StorageResult<()> {
    let ledger = StorageFinanceLedger { trx };
    let epoch = trx.finance_legacy_record().find_many(Default::default())?;
    let mut created = BTreeMap::new();
    for record in &epoch {
        let id = record.legacy_key.as_str();
        let document = &record.document;
        let (family, id, path) = match record.record_family.as_str() {
            "hold" => (FinanceDoc::Hold, id, String::new()),
            "pool" => (FinanceDoc::Pool, id, String::new()),
            "pool_reservation" => (FinanceDoc::PoolReservation, id, String::new()),
            "live_debit" => (FinanceDoc::LiveDebit, id, String::new()),
            "payout" => (FinanceDoc::Payout, id, String::new()),
            "journal_entry" => {
                if let Some(at) = document.get("createdAt").and_then(Value::as_i64) {
                    created.insert(id.to_owned(), at);
                }
                (FinanceDoc::Journal, id, String::new())
            }
            "project_budget" => (FinanceDoc::ProjectBudget, id, String::new()),
            "billing_catalog" => (FinanceDoc::BillingCatalog, id, String::new()),
            "billing_quote" => (FinanceDoc::BillingQuote, id, String::new()),
            "billing_namespace" if id == "current" => {
                (FinanceDoc::BillingCurrent, "", String::new())
            }
            "billing_namespace" if id == "nodes" => (FinanceDoc::BillingNodes, "", String::new()),
            "market_namespace" => (FinanceDoc::Market, id, String::new()),
            "token_lock" => {
                let (owner, lock) = id
                    .split_once("::")
                    .ok_or_else(|| invalid(format!("legacy token lock {id} is not owner::lock")))?;
                (FinanceDoc::Creature, owner, format!("lockedTokens.{lock}"))
            }
            "vm_billing" => {
                // A VM's billing is re-established when the VMM hands it back.
                report.notes.push(format!(
                    "VM billing of {id} stays in the legacy finance epoch"
                ));
                continue;
            }
            // Markers and counters come from the links below.
            _ => continue,
        };
        ledger
            .put_doc(family, id, &path, document, false)
            .map_err(port)?;
        report.finance_records += 1;
    }
    for record in records {
        let Some((family, rest, value)) = link(record) else {
            continue;
        };
        if MARKER_FAMILIES.contains(&family) {
            let key = format!("{family}::{rest}");
            trx.marker().upsert(
                marker::by_key(key.clone()),
                marker::Create {
                    key,
                    value: value.clone(),
                },
                marker::update().value(value),
            )?;
        } else if let Some(kind) = wallet_counter(family) {
            let amount = value
                .trim()
                .parse::<i64>()
                .map_err(|_| invalid(format!("legacy {family}::{rest} is not an integer")))?;
            ledger.set_counter(kind, rest, amount).map_err(port)?;
        } else if family == "FinancePoolByUser" {
            ledger.put_pool_of_user(rest, &value).map_err(port)?;
        } else if family == "FinanceJournalByUser" {
            let Some((user, _)) = rest.split_once("::") else {
                continue;
            };
            trx.finance_journal_participant()
                .create(finance_journal_participant::Create {
                    journal_ref: value.clone(),
                    participant_ref: user.to_owned(),
                    created_millis: created.get(&value).copied().unwrap_or(0),
                })?;
        } else {
            continue;
        }
        report.finance_records += 1;
    }
    Ok(())
}

/// Carry the legacy id counters over (`globalIdCounter`, `localIdCounter`).
fn seed_counters(trx: &aseman_storage::Trx, records: &[LegacyPhysicalRecord]) -> StorageResult<()> {
    for record in records {
        let name = match record.key.as_slice() {
            b"globalIdCounter" => "global",
            b"localIdCounter" => "local",
            _ => continue,
        };
        let value = <[u8; 8]>::try_from(record.value.as_slice())
            .map(i64::from_be_bytes)
            .map_err(|_| invalid("a legacy id counter is not 8 bytes"))?;
        trx.counter().upsert(
            counter::by_key(name),
            counter::Create {
                key: name.to_owned(),
                value,
            },
            counter::update().value(value),
        )?;
    }
    Ok(())
}

/// Convert the legacy store `source` into models of `target` (which must hold none).
///
/// # Errors
///
/// A legacy store the reviewed transform refuses, missing evidence, or a provider
/// failure.
pub fn convert_legacy(
    source: &LegacySource,
    options: &LegacyOptions,
    target: &Storage,
    report: &mut Report,
) -> StorageResult<()> {
    let records = legacy_records(source)?;
    let graph = LegacySnapshotGraph::assemble(records.clone()).map_err(legacy)?;
    let evidence = evidence(&graph, options)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
        });
    let capsules = graph
        .transform_reviewed_with_evidence(now, &evidence)
        .map_err(legacy)?;
    let schema = target.schema();
    let (pairs, capsules): (Vec<_>, Vec<_>) = capsules
        .into_iter()
        .partition(|capsule| capsule.kind.0 == LEGACY_GUEST_KV_KIND);
    let mut by_kind: BTreeMap<String, Vec<CapsuleEnvelope>> = BTreeMap::new();
    for capsule in with_keys(schema, capsules)? {
        schema.model(&capsule.kind.0).map_err(|_| {
            invalid(format!(
                "the legacy transform produced unknown kind {}",
                capsule.kind.0
            ))
        })?;
        by_kind
            .entry(capsule.kind.0.clone())
            .or_default()
            .push(capsule);
    }
    if let Some(signal_log) = &options.signal_log {
        let rows = read_legacy_signals(signal_log).map_err(legacy)?;
        let policies = rows
            .iter()
            .map(|row| {
                let policy =
                    aseman_contracts::signals::SignalStreamPolicy::for_store(&row.store_id);
                (
                    row.store_id.clone(),
                    LegacySignalStreamPolicy {
                        authorization_scope: policy.authorization_scope,
                        retention_class: policy.retention_class,
                    },
                )
            })
            .collect();
        for event in transform_legacy_signal_rows(rows, &policies).map_err(legacy)? {
            by_kind.entry(event.kind.0.clone()).or_default().push(event);
        }
    }
    for model in dependency_order(schema) {
        let Some(capsules) = by_kind.remove(&model.name) else {
            continue;
        };
        for page in capsules.chunks(PAGE) {
            target.provider().import(model, page)?;
        }
        report
            .models
            .insert(model.name.clone(), capsules.len() as u64);
    }
    let trx = target.begin(Mode::ReadWrite)?;
    for capsule in &pairs {
        let (creature, namespace, name, value) = guest_pair(capsule)?;
        let namespace = guest_kv::namespace(&namespace)
            .ok_or_else(|| invalid(format!("unknown guest namespace {namespace}")))?;
        guest_kv::put_pair(&trx, creature, namespace, &name, &value)?;
        report.guest_pairs += 1;
    }
    bridge_finance(&trx, &records, report)?;
    seed_counters(&trx, &records)?;
    trx.commit()
}

// ───────────────────────────── the command ─────────────────────────────

/// `storage migrate` usage.
pub const USAGE: &str = "\
Usage: storage migrate [--to rocksdb|postgres] [--database-url-secret FILE]
                       [--shards-secret FILE] [--file-artifact ID=PATH]...
                       [--signal-log questdb|postgres|none] [--dry-run]

Converts a store from before ADR 0036 into models, in place or into --to, and copies
every model and consensus log into --to when it names another provider. Run it
while the node is stopped. --database-url-secret and --shards-secret name the
target PostgreSQL's secrets when they differ from the configured ones. The legacy
signal history is read from --signal-log (default: ASEMAN_SIGNAL_LOG_PROVIDER);
`none` leaves it behind.";

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let long = format!("--{name}");
    let long_eq = format!("--{name}=");
    args.iter().enumerate().find_map(|(index, arg)| {
        if *arg == long {
            args.get(index + 1).map(String::as_str)
        } else {
            arg.strip_prefix(&long_eq)
        }
    })
}

fn flags<'a>(args: &'a [String], name: &str) -> Vec<&'a str> {
    let long = format!("--{name}");
    let long_eq = format!("--{name}=");
    let mut values = Vec::new();
    for (index, arg) in args.iter().enumerate() {
        if *arg == long {
            values.extend(args.get(index + 1).map(String::as_str));
        } else if let Some(value) = arg.strip_prefix(&long_eq) {
            values.push(value);
        }
    }
    values
}

/// Where the legacy signal history is: `--signal-log questdb|postgres|none`, else the
/// configured `ASEMAN_SIGNAL_LOG_PROVIDER`.
fn signal_log_source(
    config: &AsemanConfig,
    args: &[String],
) -> StorageResult<Option<LegacySignalLogSource>> {
    let questdb = || LegacySignalLogSource::QuestDb {
        port: config.services.questdb_port,
    };
    let postgres = || -> StorageResult<LegacySignalLogSource> {
        let url = settings(config, &SecretOverrides::default())?
            .database_url
            .ok_or_else(|| invalid("the PostgreSQL signal log needs a database URL"))?;
        Ok(LegacySignalLogSource::Postgres { url })
    };
    Ok(match flag(args, "signal-log") {
        Some("none") => None,
        Some("questdb") => Some(questdb()),
        Some("postgres") => Some(postgres()?),
        Some(other) => return Err(invalid(format!("unknown signal log {other}"))),
        None => match config.core_storage.signal_log {
            aseman_config::SignalLogProvider::QuestDb => Some(questdb()),
            aseman_config::SignalLogProvider::Postgres => Some(postgres()?),
        },
    })
}

fn open(name: &str, settings: &ProviderSettings) -> StorageResult<Storage> {
    // Opened directly: a store with a legacy layout is exactly what migrates.
    let provider: Arc<dyn StorageProvider> = registry().open(name, settings)?;
    Ok(Storage::new(provider, settings.schema.clone()))
}

fn legacy_source(name: &str, settings: &ProviderSettings) -> StorageResult<LegacySource> {
    if name == aseman_storage_rocksdb::model_store::NAME {
        return settings
            .legacy_store
            .clone()
            .map(LegacySource::RocksDb)
            .ok_or_else(|| invalid("no legacy store is configured"));
    }
    settings
        .database_url
        .clone()
        .map(LegacySource::Postgres)
        .ok_or_else(|| invalid("the postgres provider needs a database URL"))
}

/// Whether the legacy layout is a legacy key/value store (not only consensus logs).
fn has_legacy_records(name: &str, settings: &ProviderSettings) -> StorageResult<bool> {
    Ok(match legacy_source(name, settings)? {
        LegacySource::RocksDb(path) => path.join("CURRENT").is_file(),
        LegacySource::Postgres(url) => {
            !aseman_storage_postgres::compatibility::legacy_records(&url)
                .map_err(legacy)?
                .is_empty()
        }
    })
}

/// Run `storage migrate` for `config`; the report as text.
///
/// # Errors
///
/// Bad arguments, a target that is not empty, or a failed conversion or copy.
pub fn command(config: &AsemanConfig, args: &[String]) -> StorageResult<String> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(USAGE.to_owned());
    }
    let source_name = provider_name(config.core_storage.provider);
    let target_name = flag(args, "to").unwrap_or(source_name);
    if !registry().names().any(|name| name == target_name) {
        return Err(invalid(format!("unknown storage provider {target_name}")));
    }
    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let mut file_artifacts = BTreeMap::new();
    for pair in flags(args, "file-artifact") {
        let (id, path) = pair
            .split_once('=')
            .ok_or_else(|| invalid("--file-artifact takes ID=PATH"))?;
        file_artifacts.insert(id.to_owned(), PathBuf::from(path));
    }
    let storage_root = PathBuf::from(&config.storage.root_path);
    let options = LegacyOptions {
        storage_root: storage_root.clone(),
        currency: aseman_domain::creature::BALANCE_CURRENCY.to_owned(),
        scale: aseman_domain::creature::BALANCE_SCALE,
        local_origins: BTreeSet::from(["global".to_owned(), config.node.id.clone()]),
        file_artifacts,
        signal_log: signal_log_source(config, args)?,
    };

    let source_settings = settings(config, &SecretOverrides::default())?;
    let source = open(source_name, &source_settings)?;
    let legacy_layout = source.provider().legacy_layout()?;
    let in_place = target_name == source_name;
    let target_settings = if in_place {
        source_settings.clone()
    } else {
        settings(
            config,
            &SecretOverrides {
                database_url_secret: flag(args, "database-url-secret").map(PathBuf::from),
                shards_secret: flag(args, "shards-secret").map(PathBuf::from),
            },
        )?
    };
    if in_place && legacy_layout.is_none() {
        return Ok(format!(
            "the {source_name} store is already in the model layout; nothing to migrate\n"
        ));
    }
    let mut plan = String::new();
    if let Some(layout) = &legacy_layout {
        plan.push_str(&format!("convert the {layout} layout into models\n"));
    }
    if !in_place {
        plan.push_str(&format!(
            "copy every model and consensus log from {source_name} to {target_name}\n"
        ));
    }
    if dry_run {
        return Ok(format!("dry run; would:\n{plan}"));
    }

    let target = if in_place {
        source.clone()
    } else {
        open(target_name, &target_settings)?
    };
    ensure_empty(&target)?;
    let mut report = Report::default();
    if legacy_layout.is_some() {
        if has_legacy_records(source_name, &source_settings)? {
            convert_legacy(
                &legacy_source(source_name, &source_settings)?,
                &options,
                &target,
                &mut report,
            )?;
        }
        relocate_legacy_logs(
            source_name,
            source.provider().consensus_logs().as_ref(),
            &storage_root,
            target.provider().consensus_logs().as_ref(),
            config.services.rocksdb,
            &mut report,
        )?;
    }
    if in_place {
        // The legacy layout is converted: set it aside (kept for inspection).
        for name in source.provider().consensus_logs().names().map_err(port)? {
            if name.starts_with('/') {
                source
                    .provider()
                    .consensus_logs()
                    .open(&name, true)
                    .map_err(port)?;
            }
        }
        source.provider().retire_legacy_layout()?;
    } else {
        copy_models(&source, &target, &mut report)?;
        copy_consensus_logs(
            source.provider().consensus_logs().as_ref(),
            target.provider().consensus_logs().as_ref(),
            &mut report,
        )?;
        report.notes.push(format!(
            "the {source_name} store is unchanged (the rollback); set \
             ASEMAN_CORE_STORAGE_PROVIDER={target_name} to run the node on {target_name}"
        ));
    }
    Ok(format!("{plan}done:\n{report}"))
}

#[cfg(test)]
mod tests;
