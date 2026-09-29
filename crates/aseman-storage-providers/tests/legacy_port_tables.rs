//! The tables the PostgreSQL-only port adapters kept before ADR 0038 are imported
//! when PostgreSQL storage opens: the ports read the imported state, sequences and
//! fencing tokens continue past it, the legacy tables move to `aseman_retired`, and a
//! second open imports nothing.

mod support;

use std::collections::BTreeMap;

use aseman_capsule::coordination::StorageCoordination;
use aseman_capsule::federation::StorageFederation;
use aseman_capsule::metering::StorageMetering;
use aseman_capsule::realtime::StorageRealtime;
use aseman_capsule::vmm::StorageVmmStore;
use aseman_domain::coordination::{Acquisition, LeaseName};
use aseman_domain::federation::NodeDescriptor;
use aseman_domain::finance::{
    Dimension, Minor, PriceList, UsageSample, interval_between, price, settle,
};
use aseman_domain::vmm::{WorkloadEventRecord, WorkloadEventType};
use aseman_domain::{OperationId, Uuid, WorkloadId};
use aseman_ports::conformance::vmm::sample_workload;
use aseman_ports::coordination::CoordinationPort;
use aseman_ports::federation::Directory;
use aseman_ports::finance::{Ledger, PricingStore, UsageStore};
use aseman_ports::realtime::{CheckpointStore, EventLog, Outbox};
use aseman_ports::vmm::{IdempotencyClaim, IdempotencyStore, VmmEventLog, VmmWorkloadStore};

const LEGACY_TABLES: &str = include_str!("fixtures/legacy-port-tables.sql");

fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn json<T: serde::Serialize>(value: &T) -> String {
    quoted(&serde_json::to_string(value).unwrap())
}

fn sample(workload: Uuid, id: &str, at: i64, cpu: u64) -> UsageSample {
    UsageSample {
        workload_id: workload,
        provider_sample_id: id.to_owned(),
        provider: "nomad".to_owned(),
        collected_at_millis: at,
        cumulative: BTreeMap::from([(Dimension::CpuMillis, cpu)]),
    }
}

#[test]
fn legacy_port_tables_are_imported_and_retired() {
    let Some(database) = support::FreshDatabase::create() else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping legacy import test");
        return;
    };
    let mut legacy = database.client();
    legacy.batch_execute(LEGACY_TABLES).unwrap();

    // Realtime: one unpublished event and a checkpoint.
    let event = Uuid::now_v7();
    let creature = Uuid::now_v7();
    legacy
        .batch_execute(&format!(
            "INSERT INTO aseman_core.realtime_event VALUES ('{event}', 'legacy', 1, \
             '{creature}', 'legacy.tick', 'node', 1000, {digest}, 'durable', '1', NULL, \
             '\\x0102'::bytea);
             INSERT INTO aseman_core.realtime_outbox (event_id) VALUES ('{event}');
             INSERT INTO aseman_core.realtime_checkpoint VALUES ('consumer', 'legacy', 1, 1000);",
            digest = quoted(&format!("sha256:{}", "d".repeat(64))),
        ))
        .unwrap();

    // Metering: a price list, two samples, their interval, and its settlement.
    let prices = PriceList {
        version: "2026-09".to_owned(),
        effective_from_millis: 0,
        rates: BTreeMap::from([(Dimension::CpuMillis, 1_000_000)]),
    };
    let workload = Uuid::now_v7();
    let first = sample(workload, "s-1", 0, 0);
    let second = sample(workload, "s-2", 60_000, 30_000);
    let interval = interval_between(&first, &second).unwrap();
    let record = settle(
        &price(&interval, &prices).unwrap(),
        "wallet:alice",
        "revenue:compute",
        60_000,
    )
    .unwrap();
    let mut statements = format!(
        "INSERT INTO aseman_core.price_list VALUES ('2026-09', 0, {});",
        json(&prices)
    );
    for sample in [&first, &second] {
        statements.push_str(&format!(
            "INSERT INTO aseman_core.usage_sample VALUES ('{workload}', {}, 'nomad', {}, {});",
            quoted(&sample.provider_sample_id),
            sample.collected_at_millis,
            json(sample)
        ));
    }
    statements.push_str(&format!(
        "INSERT INTO aseman_core.usage_interval VALUES ({}, '{workload}', {}, {}, {});
         INSERT INTO aseman_core.journal_record VALUES ({}, {}, '2026-09', {});",
        quoted(&interval.settlement_key()),
        interval.interval_start_millis,
        interval.interval_end_millis,
        json(&interval),
        quoted(&record.idempotency_key),
        record.at_millis,
        json(&record),
    ));
    for (ordinal, entry) in record.entries.iter().enumerate() {
        statements.push_str(&format!(
            "INSERT INTO aseman_core.journal_entry VALUES ({}, {ordinal}, {}, {});",
            quoted(&record.idempotency_key),
            quoted(&entry.account),
            entry.amount.0
        ));
    }
    legacy.batch_execute(&statements).unwrap();

    // Federation: a peer's descriptor.
    let peer = Uuid::now_v7();
    let descriptor = NodeDescriptor {
        node_id: peer,
        key_epoch: 2,
        keys: vec!["key".to_owned()],
        federation_endpoint: "https://peer.invalid/federation".to_owned(),
        client_endpoint: "https://peer.invalid".to_owned(),
        contracts: vec!["a501/1".to_owned()],
        runtimes: vec!["docker".to_owned()],
        sequence: 5,
        expires_at_millis: i64::MAX,
        revoked_epochs: Vec::new(),
    };
    legacy
        .batch_execute(&format!(
            "INSERT INTO aseman_core.federation_node VALUES ('{peer}', 5, {}, {});",
            i64::MAX,
            json(&descriptor)
        ))
        .unwrap();

    // Coordination: an expired lease whose token is at 5.
    legacy
        .batch_execute(
            "INSERT INTO aseman_core.coordination_lease VALUES ('job', 'old', 5, 0, 0);
             INSERT INTO aseman_core.coordination_fence VALUES ('job', 5);",
        )
        .unwrap();

    // VMM: a workload, a completed idempotency claim, and an event log at sequence 7
    // truncated through 3.
    let workload_id = WorkloadId::new();
    let record = sample_workload("node-a", workload_id, "wasm");
    let event = WorkloadEventRecord {
        owner: "node-a".to_owned(),
        sequence: 7,
        workload_id,
        at_millis: 1_000,
        event_type: WorkloadEventType::Operation,
        observation: None,
        operation: Some(OperationId::new()),
    };
    legacy
        .batch_execute(&format!(
            "INSERT INTO aseman_vmm.workload VALUES ('{workload_id}', 'node-a', '{}', NULL, \
             {}, {}::jsonb);
             INSERT INTO aseman_vmm.idempotency VALUES ('node-a', 'create-1', \
             decode(repeat('ab', 32), 'hex'), 1000, 201, '\\x7b7d'::bytea, \
             'application/json', '/v1/workloads/{workload_id}');
             INSERT INTO aseman_vmm.event (sequence, owner, workload_id, record, at_millis) \
             VALUES (7, 'node-a', '{workload_id}', {}::jsonb, 1000);
             UPDATE aseman_vmm.event_log SET last_sequence = 7, truncated_through = 3;",
            record.labels.creature_id,
            record.resource_version,
            json(&record),
            json(&event),
        ))
        .unwrap();

    let storage = aseman_storage_providers::open_database(database.url.clone(), 4).unwrap();

    let realtime = StorageRealtime::new(storage.clone());
    let events = realtime.read("legacy", 0, 10).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload, vec![1, 2]);
    let claim = realtime.claim("worker", 10, 60_000).unwrap();
    assert_eq!(
        claim.events.len(),
        1,
        "the unpublished event is still in the outbox"
    );
    assert_eq!(
        realtime
            .checkpoint("consumer", "legacy")
            .unwrap()
            .map(|checkpoint| checkpoint.sequence),
        Some(1)
    );

    let metering = StorageMetering::new(storage.clone());
    assert_eq!(metering.price_lists().unwrap(), vec![prices]);
    assert_eq!(metering.balance("wallet:alice").unwrap(), Minor(-30));
    assert!(metering.unsettled(10).unwrap().is_empty());
    assert_eq!(
        metering.previous_sample(workload, 60_000).unwrap(),
        Some(first)
    );

    let federation = StorageFederation::new(storage.clone(), Uuid::now_v7());
    assert_eq!(federation.node(peer, 1_000).unwrap(), Some(descriptor));

    let coordination = StorageCoordination::new(storage.clone());
    match coordination
        .acquire(&LeaseName::new("job").unwrap(), "new", 60_000)
        .unwrap()
    {
        Acquisition::Granted(lease) => {
            assert!(lease.token.get() > 5, "tokens continue past the legacy one");
        }
        other => panic!("the expired legacy lease is free: {other:?}"),
    }

    let vmm = StorageVmmStore::new(storage.clone());
    assert_eq!(vmm.workload("node-a", workload_id).unwrap(), Some(record));
    assert!(matches!(
        vmm.claim("node-a", "create-1", [0xab; 32], 2_000, 60_000)
            .unwrap(),
        IdempotencyClaim::Completed(response) if response.status == 201
    ));
    assert!(vmm.events_after("node-a", 2, None, 10).unwrap().resync);
    assert_eq!(
        vmm.events_after("node-a", 3, None, 10).unwrap().events,
        vec![event.clone()]
    );
    let mut next = event;
    next.sequence = 0;
    assert_eq!(vmm.append(&next).unwrap(), 8, "sequences continue");

    // Every legacy table moved, unchanged, to the retired schema.
    let placed: Vec<(String, i64)> = legacy
        .query(
            "SELECT table_schema, count(*) FROM information_schema.tables \
             WHERE table_schema IN ('aseman_retired', 'aseman_vmm') \
                OR (table_schema = 'aseman_core' AND table_name IN \
                    ('realtime_event', 'usage_sample', 'federation_node', 'coordination_lease')) \
             GROUP BY table_schema",
            &[],
        )
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(placed, vec![("aseman_retired".to_owned(), 19)]);
    let retired: i64 = legacy
        .query_one("SELECT count(*) FROM aseman_retired.vmm_workload", &[])
        .unwrap()
        .get(0);
    assert_eq!(retired, 1);

    // A second open finds nothing left to import.
    drop(storage);
    assert_eq!(
        aseman_storage_providers::port_tables::import(
            &aseman_storage_providers::open_database(database.url.clone(), 2).unwrap(),
            &database.url,
        )
        .unwrap(),
        0
    );
}
