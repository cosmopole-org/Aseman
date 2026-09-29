//! The one-time import of the tables the PostgreSQL-only port adapters kept before
//! ADR 0038 (realtime, metering, federation, coordination, and the VMM service's
//! `aseman_vmm` schema) into the storage module's models.
//!
//! It runs when a PostgreSQL storage opens. Each legacy table is imported in batches
//! (a row whose key is already stored is skipped, so an interrupted import resumes),
//! and once imported it is moved, untouched, into the `aseman_retired` schema: the
//! rollback source until an operator drops it. A database with
//! no legacy tables costs one catalog query.

use aseman_storage::client::core::{
    coordination_fence, coordination_lease, counter, federation_answer, federation_node_descriptor,
    federation_nonce, federation_workload_descriptor, journal_entry, journal_record, price_list,
    realtime_checkpoint, realtime_log_event, realtime_outbox_entry, usage_interval, usage_sample,
    vmm_event, vmm_idempotency, vmm_operation, vmm_workload,
};
use aseman_storage::{Mode, Models, Storage, StorageError, StorageResult, Trx};
use postgres::{Client, Row};

/// Rows written per storage transaction.
const BATCH: usize = 500;

/// The counters the VMM event log keeps its sequence in (`aseman_capsule::vmm`).
const LAST_SEQUENCE: &str = "vmm.event.last_sequence";
const TRUNCATED_THROUGH: &str = "vmm.event.truncated_through";

fn failed(error: impl std::fmt::Display) -> StorageError {
    StorageError::invalid(format!("legacy port table import: {error}"))
}

/// Import every legacy port table in the database at `database_url`; how many rows
/// were imported.
///
/// # Errors
///
/// An unreachable database, an unreadable legacy row, or a failed write. Nothing is
/// retired unless its import committed.
pub fn import(storage: &Storage, database_url: &str) -> StorageResult<u64> {
    let mut client = aseman_postgres::Database::parse(database_url)
        .map_err(failed)?
        .connect()
        .map_err(failed)?;
    let present = legacy_tables(&mut client)?;
    if present.is_empty() {
        return Ok(0);
    }
    let mut imported = 0;
    for group in GROUPS {
        if !group
            .tables
            .iter()
            .any(|table| present.contains(&(*table).to_owned()))
        {
            continue;
        }
        let rows = (group.import)(&mut client, storage)?;
        retire(&mut client, group.tables, &present)?;
        if rows > 0 {
            eprintln!("[storage] imported {rows} legacy {} row(s)", group.name);
        }
        imported += rows;
    }
    Ok(imported)
}

struct Group {
    name: &'static str,
    /// Retired together, dependents first.
    tables: &'static [&'static str],
    import: fn(&mut Client, &Storage) -> StorageResult<u64>,
}

const GROUPS: &[Group] = &[
    Group {
        name: "realtime",
        tables: &[
            "aseman_core.realtime_outbox",
            "aseman_core.realtime_event",
            "aseman_core.realtime_checkpoint",
        ],
        import: realtime,
    },
    Group {
        name: "metering",
        tables: &[
            "aseman_core.journal_entry",
            "aseman_core.journal_record",
            "aseman_core.usage_interval",
            "aseman_core.usage_sample",
            "aseman_core.price_list",
        ],
        import: metering,
    },
    Group {
        name: "federation",
        tables: &[
            "aseman_core.federation_node",
            "aseman_core.federation_workload",
            "aseman_core.federation_nonce",
            "aseman_core.federation_answer",
        ],
        import: federation,
    },
    Group {
        name: "coordination",
        tables: &[
            "aseman_core.coordination_lease",
            "aseman_core.coordination_fence",
        ],
        import: coordination,
    },
    Group {
        name: "VMM",
        tables: &[
            "aseman_vmm.workload",
            "aseman_vmm.operation",
            "aseman_vmm.idempotency",
            "aseman_vmm.event",
            "aseman_vmm.event_log",
        ],
        import: vmm,
    },
];

fn legacy_tables(client: &mut Client) -> StorageResult<Vec<String>> {
    let names: Vec<&str> = GROUPS
        .iter()
        .flat_map(|group| group.tables.iter().copied())
        .collect();
    Ok(client
        .query(
            "SELECT n.nspname || '.' || c.relname FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind = 'r' AND n.nspname || '.' || c.relname = ANY($1)",
            &[&names],
        )
        .map_err(failed)?
        .into_iter()
        .map(|row| row.get(0))
        .collect())
}

fn retire(client: &mut Client, tables: &[&str], present: &[String]) -> StorageResult<()> {
    let mut statement = String::from("CREATE SCHEMA IF NOT EXISTS aseman_retired;");
    for table in tables
        .iter()
        .filter(|table| present.contains(&(**table).to_owned()))
    {
        let (schema, name) = table.split_once('.').expect("qualified");
        // The VMM schema's short names are prefixed so they stay distinct.
        let retired = if schema == "aseman_vmm" {
            format!("vmm_{name}")
        } else {
            name.to_owned()
        };
        if retired != name {
            statement.push_str(&format!("ALTER TABLE {table} RENAME TO {retired};"));
        }
        statement.push_str(&format!(
            "ALTER TABLE {schema}.{retired} SET SCHEMA aseman_retired;"
        ));
    }
    client.batch_execute(&statement).map_err(failed)
}

/// Write `rows` in batches; `write` returns whether it stored the row (false when
/// its key is already there).
fn write_batches(
    storage: &Storage,
    rows: Vec<Row>,
    write: impl Fn(&Trx, &Row) -> StorageResult<bool>,
) -> StorageResult<u64> {
    let mut written = 0;
    for chunk in rows.chunks(BATCH) {
        let trx = storage.begin(Mode::ReadWrite)?;
        for row in chunk {
            if write(&trx, row)? {
                written += 1;
            }
        }
        trx.commit()?;
    }
    Ok(written)
}

fn select(client: &mut Client, sql: &str) -> StorageResult<Vec<Row>> {
    client.query(sql, &[]).map_err(failed)
}

fn json(text: &str) -> StorageResult<serde_json::Value> {
    serde_json::from_str(text).map_err(failed)
}

fn realtime(client: &mut Client, storage: &Storage) -> StorageResult<u64> {
    let events = select(
        client,
        "SELECT e.id::text, e.stream, e.sequence, e.creature_id::text, e.kind, e.producer, \
                e.at_millis, e.payload_digest, e.retention, e.version, e.idempotency_key, \
                e.payload, o.claimed_by, o.claimed_until_millis, o.attempts::bigint, o.published \
         FROM aseman_core.realtime_event e \
         LEFT JOIN aseman_core.realtime_outbox o ON o.event_id = e.id",
    )?;
    let mut written = write_batches(storage, events, |trx, row| {
        let key: String = row.get(0);
        if trx
            .realtime_log_event()
            .find_unique(realtime_log_event::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.realtime_log_event()
            .create(realtime_log_event::Create {
                key: key.clone(),
                stream: row.get(1),
                sequence: row.get(2),
                creature_id: row.get(3),
                event_kind: row.get(4),
                producer: row.get(5),
                at_millis: row.get(6),
                payload_digest: row.get(7),
                retention: row.get(8),
                version: row.get(9),
                idempotency_key: row.get(10),
                payload: row.get(11),
            })?;
        // An event without an outbox row was already published and cleaned up.
        if let Some(attempts) = row.get::<_, Option<i64>>(14) {
            trx.realtime_outbox_entry()
                .create(realtime_outbox_entry::Create {
                    key,
                    claimed_by: row.get(12),
                    claimed_until_millis: row.get(13),
                    attempts,
                    published: row.get(15),
                })?;
        }
        Ok(true)
    })?;
    let checkpoints = select(
        client,
        "SELECT consumer, stream, sequence, at_millis FROM aseman_core.realtime_checkpoint",
    )?;
    written += write_batches(storage, checkpoints, |trx, row| {
        let consumer: String = row.get(0);
        let stream: String = row.get(1);
        let key = format!("{consumer}::{stream}");
        if trx
            .realtime_checkpoint()
            .find_unique(realtime_checkpoint::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.realtime_checkpoint()
            .create(realtime_checkpoint::Create {
                key,
                consumer,
                stream,
                sequence: row.get(2),
                at_millis: row.get(3),
            })?;
        Ok(true)
    })?;
    Ok(written)
}

fn metering(client: &mut Client, storage: &Storage) -> StorageResult<u64> {
    let samples = select(
        client,
        "SELECT workload_id::text, provider_sample_id, provider, collected_at_millis, sample \
         FROM aseman_core.usage_sample",
    )?;
    let mut written = write_batches(storage, samples, |trx, row| {
        let workload: String = row.get(0);
        let provider_sample_id: String = row.get(1);
        let key = format!("{workload}::{provider_sample_id}");
        if trx
            .usage_sample()
            .find_unique(usage_sample::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.usage_sample().create(usage_sample::Create {
            key,
            workload_id: workload,
            provider_sample_id,
            provider: row.get(2),
            collected_at_millis: row.get(3),
            sample: row.get(4),
        })?;
        Ok(true)
    })?;
    let intervals = select(
        client,
        "SELECT settlement_key, workload_id::text, interval_start_millis, \
                interval_end_millis, interval \
         FROM aseman_core.usage_interval",
    )?;
    written += write_batches(storage, intervals, |trx, row| {
        let key: String = row.get(0);
        if trx
            .usage_interval()
            .find_unique(usage_interval::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.usage_interval().create(usage_interval::Create {
            key,
            workload_id: row.get(1),
            interval_start_millis: row.get(2),
            interval_end_millis: row.get(3),
            interval: row.get(4),
        })?;
        Ok(true)
    })?;
    let prices = select(
        client,
        "SELECT version, effective_from_millis, list FROM aseman_core.price_list",
    )?;
    written += write_batches(storage, prices, |trx, row| {
        let key: String = row.get(0);
        if trx
            .price_list()
            .find_unique(price_list::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.price_list().create(price_list::Create {
            key,
            effective_from_millis: row.get(1),
            list: row.get(2),
        })?;
        Ok(true)
    })?;
    // A journal record and its entries are imported in one transaction: a record is
    // never visible without the entries that make it balance.
    let records = select(
        client,
        "SELECT idempotency_key, at_millis, price_version, record FROM aseman_core.journal_record",
    )?;
    let entries = select(
        client,
        "SELECT idempotency_key, ordinal::bigint, account, amount FROM aseman_core.journal_entry",
    )?;
    let mut by_record = std::collections::BTreeMap::<String, Vec<&Row>>::new();
    for entry in &entries {
        by_record.entry(entry.get(0)).or_default().push(entry);
    }
    written += write_batches(storage, records, |trx, row| {
        let key: String = row.get(0);
        if trx
            .journal_record()
            .find_unique(journal_record::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.journal_record().create(journal_record::Create {
            key: key.clone(),
            at_millis: row.get(1),
            price_version: row.get(2),
            record: row.get(3),
        })?;
        for entry in by_record.get(&key).into_iter().flatten() {
            let ordinal: i64 = entry.get(1);
            trx.journal_entry().create(journal_entry::Create {
                key: format!("{key}::{ordinal}"),
                record_key: key.clone(),
                ordinal,
                account: entry.get(2),
                amount: entry.get(3),
            })?;
        }
        Ok(true)
    })?;
    Ok(written)
}

fn federation(client: &mut Client, storage: &Storage) -> StorageResult<u64> {
    let nodes = select(
        client,
        "SELECT node_id::text, sequence, expires_at_millis, descriptor \
         FROM aseman_core.federation_node",
    )?;
    let mut written = write_batches(storage, nodes, |trx, row| {
        let key: String = row.get(0);
        if trx
            .federation_node_descriptor()
            .find_unique(federation_node_descriptor::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.federation_node_descriptor()
            .create(federation_node_descriptor::Create {
                key,
                sequence: row.get(1),
                expires_at_millis: row.get(2),
                descriptor: row.get(3),
            })?;
        Ok(true)
    })?;
    let workloads = select(
        client,
        "SELECT workload_id::text, revision, expires_at_millis, descriptor \
         FROM aseman_core.federation_workload",
    )?;
    written += write_batches(storage, workloads, |trx, row| {
        let key: String = row.get(0);
        if trx
            .federation_workload_descriptor()
            .find_unique(federation_workload_descriptor::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.federation_workload_descriptor()
            .create(federation_workload_descriptor::Create {
                key,
                workload_revision: row.get(1),
                expires_at_millis: row.get(2),
                descriptor: row.get(3),
            })?;
        Ok(true)
    })?;
    let nonces = select(
        client,
        "SELECT source_node::text || '::' || nonce, expires_at_millis \
         FROM aseman_core.federation_nonce",
    )?;
    written += write_batches(storage, nonces, |trx, row| {
        let key: String = row.get(0);
        if trx
            .federation_nonce()
            .find_unique(federation_nonce::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.federation_nonce().create(federation_nonce::Create {
            key,
            expires_at_millis: row.get(1),
        })?;
        Ok(true)
    })?;
    let answers = select(
        client,
        "SELECT request_id::text, answer, expires_at_millis FROM aseman_core.federation_answer",
    )?;
    written += write_batches(storage, answers, |trx, row| {
        let key: String = row.get(0);
        if trx
            .federation_answer()
            .find_unique(federation_answer::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.federation_answer().create(federation_answer::Create {
            key,
            answer: row.get(1),
            expires_at_millis: row.get(2),
        })?;
        Ok(true)
    })?;
    Ok(written)
}

fn coordination(client: &mut Client, storage: &Storage) -> StorageResult<u64> {
    let leases = select(
        client,
        "SELECT name, instance, token, acquired_at_millis, expires_at_millis \
         FROM aseman_core.coordination_lease",
    )?;
    let mut written = write_batches(storage, leases, |trx, row| {
        let key: String = row.get(0);
        if trx
            .coordination_lease()
            .find_unique(coordination_lease::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.coordination_lease()
            .create(coordination_lease::Create {
                key,
                instance: row.get(1),
                token: row.get(2),
                acquired_at_millis: row.get(3),
                expires_at_millis: row.get(4),
            })?;
        Ok(true)
    })?;
    let fences = select(
        client,
        "SELECT name, token FROM aseman_core.coordination_fence",
    )?;
    written += write_batches(storage, fences, |trx, row| {
        let key: String = row.get(0);
        if trx
            .coordination_fence()
            .find_unique(coordination_fence::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.coordination_fence()
            .create(coordination_fence::Create {
                key,
                token: row.get(1),
            })?;
        Ok(true)
    })?;
    Ok(written)
}

fn vmm(client: &mut Client, storage: &Storage) -> StorageResult<u64> {
    let workloads = select(
        client,
        "SELECT id::text, owner, creature_id::text, observed_state, resource_version, \
                record::text \
         FROM aseman_vmm.workload",
    )?;
    let mut written = write_batches(storage, workloads, |trx, row| {
        let key: String = row.get(0);
        if trx
            .vmm_workload()
            .find_unique(vmm_workload::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.vmm_workload().create(vmm_workload::Create {
            key,
            owner: row.get(1),
            creature_id: row.get(2),
            observed_state: row.get(3),
            resource_version: row.get(4),
            record: json(row.get(5))?,
        })?;
        Ok(true)
    })?;
    let operations = select(
        client,
        "SELECT id::text, owner, workload_id::text, state, created_at_millis, record::text \
         FROM aseman_vmm.operation",
    )?;
    written += write_batches(storage, operations, |trx, row| {
        let key: String = row.get(0);
        if trx
            .vmm_operation()
            .find_unique(vmm_operation::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.vmm_operation().create(vmm_operation::Create {
            key,
            owner: row.get(1),
            workload_id: row.get(2),
            state: row.get(3),
            created_at_millis: row.get(4),
            record: json(row.get(5))?,
        })?;
        Ok(true)
    })?;
    let claims = select(
        client,
        "SELECT owner, key, digest, claimed_at_millis, response_status::bigint, \
                response_body, response_content_type, response_location \
         FROM aseman_vmm.idempotency",
    )?;
    written += write_batches(storage, claims, |trx, row| {
        let owner: String = row.get(0);
        let request_key: String = row.get(1);
        let key = format!("{owner}\u{1f}{request_key}");
        if trx
            .vmm_idempotency()
            .find_unique(vmm_idempotency::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.vmm_idempotency().create(vmm_idempotency::Create {
            key,
            owner,
            request_key,
            digest: row.get(2),
            claimed_at_millis: row.get(3),
            response_status: row.get(4),
            response_body: row.get(5),
            response_content_type: row.get(6),
            response_location: row.get(7),
        })?;
        Ok(true)
    })?;
    let events = select(
        client,
        "SELECT sequence, owner, workload_id::text, at_millis, record::text \
         FROM aseman_vmm.event",
    )?;
    written += write_batches(storage, events, |trx, row| {
        let sequence: i64 = row.get(0);
        let key = format!("{sequence:020}");
        if trx
            .vmm_event()
            .find_unique(vmm_event::by_key(key.clone()))?
            .is_some()
        {
            return Ok(false);
        }
        trx.vmm_event().create(vmm_event::Create {
            key,
            sequence,
            owner: row.get(1),
            workload_id: row.get(2),
            at_millis: row.get(3),
            record: json(row.get(4))?,
        })?;
        Ok(true)
    })?;
    let log = select(
        client,
        "SELECT last_sequence, truncated_through FROM aseman_vmm.event_log",
    )?;
    let trx = storage.begin(Mode::ReadWrite)?;
    for row in &log {
        raise_counter(&trx, LAST_SEQUENCE, row.get(0))?;
        raise_counter(&trx, TRUNCATED_THROUGH, row.get(1))?;
    }
    trx.commit()?;
    Ok(written)
}

/// Sequences never go back: the counter keeps the higher of what it holds and `value`.
fn raise_counter(trx: &Trx, key: &str, value: i64) -> StorageResult<()> {
    match trx.counter().find_unique(counter::by_key(key))? {
        Some(current) if current.value >= value => {}
        Some(_) => {
            trx.counter()
                .update(counter::by_key(key), counter::update().value(value))?;
        }
        None => {
            trx.counter().create(counter::Create {
                key: key.to_owned(),
                value,
            })?;
        }
    }
    Ok(())
}
