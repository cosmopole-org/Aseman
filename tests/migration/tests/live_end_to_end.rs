//! A309 end to end: legacy snapshot -> reviewed transforms -> canonical export ->
//! PostgreSQL and per-creature guest import -> semantic verification -> delta ->
//! fenced cutover -> rollback, against a disposable PostgreSQL 16.

use aseman_application::storage_migration::StorageMigrationService;
use aseman_contracts::capsule::CapsuleEnvelope;
use aseman_contracts::migration::{CanonicalCapsuleExport, CapsuleExportHeader, plan_delta};
use aseman_domain::storage_migration::{
    Authority, MigrationPhase, MigrationRecord, StorageMigration,
};
use aseman_ports::{ClockPort, MigrationRecordSource, MigrationStateStore, PortError, PortResult};
use aseman_storage_legacy::{
    CapsuleImportSink, ImportDisposition, LegacyFinanceConfig, LegacyMigrationResult,
    LegacyPhysicalRecord, LegacySnapshotGraph, LegacyTransformEvidence, import_canonical,
};
use aseman_storage_postgres::PostgresCapsuleRepository;
use aseman_storage_postgres::guest::{GuestPoolRouter, PostgresGuestProvisioner};
use aseman_storage_postgres::migration::migration_record;
use postgres::{Client, Config, NoTls};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::Mutex;

const PROXY_ROLE: &str = "aseman_migration_proxy_test";
const PROXY_PASSWORD: &str = "migration-proxy-password";

fn record(key: &str, value: impl Into<Vec<u8>>) -> LegacyPhysicalRecord {
    LegacyPhysicalRecord {
        family: "application-rocksdb-default".to_owned(),
        key: key.as_bytes().to_vec(),
        value: value.into(),
    }
}

/// Legacy `index_json` layout: the document plus one splat per non-null member.
fn put_json(
    records: &mut Vec<LegacyPhysicalRecord>,
    key: &str,
    path: &str,
    document: &serde_json::Value,
) {
    records.push(record(
        &format!("json::{key}::{path}"),
        serde_json::to_vec(document).unwrap(),
    ));
    if let serde_json::Value::Object(members) = document {
        for (member, value) in members {
            match value {
                serde_json::Value::Null => {}
                serde_json::Value::Object(_) => {
                    put_json(records, key, &format!("{path}.{member}"), value)
                }
                other => records.push(record(
                    &format!("json::{key}::{path}.{member}"),
                    serde_json::to_vec(other).unwrap(),
                )),
            }
        }
    }
}

fn legacy_snapshot(public_key: &str, bio: &str) -> Vec<LegacyPhysicalRecord> {
    let mut records = Vec::new();
    for (column, value) in [
        ("|", vec![1]),
        ("type", b"human".to_vec()),
        ("username", b"alice@node".to_vec()),
        ("publicKey", public_key.as_bytes().to_vec()),
        ("chainId", b"main".to_vec()),
        ("subchainId", b"main".to_vec()),
        ("ownerId", b"free".to_vec()),
        ("balance", 1_000_i64.to_le_bytes().to_vec()),
    ] {
        records.push(record(&format!("obj::Creature::1@global::{column}"), value));
    }
    for (column, value) in [
        ("|", vec![1]),
        ("id", b"2@global".to_vec()),
        ("machineId", b"1@global".to_vec()),
        ("runtime", b"wasm".to_vec()),
        ("path", b"/programs/two".to_vec()),
    ] {
        records.push(record(&format!("obj::Program::2@global::{column}"), value));
    }
    for (column, value) in [
        ("|", vec![1]),
        ("tag", b"events".to_vec()),
        ("parentId", Vec::new()),
        ("isPublic", vec![0]),
        ("persHist", vec![1]),
        ("memberCount", 1_i32.to_le_bytes().to_vec()),
        ("signalCount", 0_i64.to_le_bytes().to_vec()),
    ] {
        records.push(record(&format!("obj::Store::3@global::{column}"), value));
    }
    for (key, value) in [
        ("index::Creature::username::id::alice@node", "1@global"),
        ("link::machinePrograms::1@global::2@global", "true"),
        ("link::creatorof::1@global::3@global", "true"),
        ("link::onaccess::3@global::1@global", "read,signal,manage"),
        ("link::hasaccess::1@global::3@global", "true"),
        ("link::1@global::counter", "5"),
    ] {
        records.push(record(key, value));
    }
    put_json(
        &mut records,
        "UserMeta::1@global",
        "metadata",
        &serde_json::json!({"bio": bio}),
    );
    records
}

fn transform(records: Vec<LegacyPhysicalRecord>, at: i64) -> Vec<CapsuleEnvelope> {
    let evidence = LegacyTransformEvidence {
        finance: Some(LegacyFinanceConfig {
            currency: "ASE".to_owned(),
            scale: 2,
        }),
        local_origins: BTreeSet::from(["global".to_owned()]),
        ..LegacyTransformEvidence::default()
    };
    LegacySnapshotGraph::assemble(records)
        .unwrap()
        .transform_reviewed_with_evidence(at, &evidence)
        .unwrap()
}

/// Order capsules so every relationship target is imported before its referrer.
fn dependency_order(capsules: Vec<CapsuleEnvelope>) -> Vec<CapsuleEnvelope> {
    let key = |capsule: &CapsuleEnvelope| (capsule.kind.0.clone(), capsule.id.0);
    let mut pending: BTreeMap<_, _> = capsules
        .into_iter()
        .map(|capsule| (key(&capsule), capsule))
        .collect();
    let mut ordered = Vec::new();
    while !pending.is_empty() {
        let ready = pending
            .iter()
            .filter(|(_, capsule)| {
                capsule.relationships.iter().all(|relationship| {
                    !pending.contains_key(&(
                        relationship.target_kind.0.clone(),
                        relationship.target_id.0,
                    ))
                })
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        assert!(!ready.is_empty(), "relationship cycle in export");
        for key in ready {
            ordered.push(pending.remove(&key).unwrap());
        }
    }
    ordered
}

struct Sink<'a> {
    repository: &'a PostgresCapsuleRepository,
    guest: Vec<CapsuleEnvelope>,
}

impl CapsuleImportSink for Sink<'_> {
    fn import(&mut self, capsule: CapsuleEnvelope) -> LegacyMigrationResult<ImportDisposition> {
        if capsule.kind.0 == "guest.legacy_kv" {
            self.guest.push(capsule);
            return Ok(ImportDisposition::Inserted);
        }
        let present = self
            .repository
            .get(&capsule.kind, &capsule.id)
            .unwrap()
            .is_some();
        self.repository.put(&capsule, None).unwrap();
        Ok(if present {
            ImportDisposition::AlreadyPresent
        } else {
            ImportDisposition::Inserted
        })
    }
}

#[derive(Default)]
struct MemoryState(Mutex<Option<StorageMigration>>);

impl MigrationStateStore for MemoryState {
    fn load(&self, _: &str) -> PortResult<Option<StorageMigration>> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(
        &self,
        migration: &StorageMigration,
        expected: Option<MigrationPhase>,
    ) -> PortResult<()> {
        let mut stored = self.0.lock().unwrap();
        if stored.as_ref().map(|current| current.phase) != expected {
            return Err(PortError::Conflict);
        }
        *stored = Some(migration.clone());
        Ok(())
    }
}

/// The legacy side as a record source (guest pairs are compared per creature database).
struct LegacySource(Mutex<Vec<MigrationRecord>>);

impl LegacySource {
    fn set(&self, capsules: &[CapsuleEnvelope]) {
        *self.0.lock().unwrap() = capsules
            .iter()
            .filter(|capsule| capsule.kind.0 != "guest.legacy_kv")
            .map(|capsule| migration_record(capsule).unwrap())
            .collect();
    }
}

impl MigrationRecordSource for LegacySource {
    fn snapshot(&self) -> PortResult<Vec<MigrationRecord>> {
        Ok(self.0.lock().unwrap().clone())
    }
}

struct Clock;
impl ClockPort for Clock {
    fn unix_millis(&self) -> i64 {
        1_700_000_000_000
    }
}

#[test]
fn live_legacy_migration_verifies_applies_delta_and_cuts_over_with_fencing() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping A309 end-to-end test");
        return;
    };
    // A dedicated database isolates this run from rows other live tests leave behind.
    let database = format!("aseman_migration_e2e_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    admin
        .batch_execute(&format!(
            "DO $$ BEGIN CREATE ROLE {PROXY_ROLE} LOGIN NOINHERIT; \
             EXCEPTION WHEN duplicate_object THEN NULL; END $$; \
             ALTER ROLE {PROXY_ROLE} LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
             NOREPLICATION NOBYPASSRLS PASSWORD '{PROXY_PASSWORD}';"
        ))
        .unwrap();
    let mut target_config = Config::from_str(&admin_uri).unwrap();
    target_config.dbname(&database);
    let repository = PostgresCapsuleRepository::from_client(target_config.connect(NoTls).unwrap());
    repository.migrate().unwrap();

    let public_key = {
        use rsa::pkcs8::{EncodePublicKey, LineEnding};
        let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        rsa::RsaPublicKey::from(&private)
            .to_public_key_pem(LineEnding::LF)
            .unwrap()
    };
    let initial = dependency_order(transform(legacy_snapshot(&public_key, "hello"), 1_000));
    let kinds = initial
        .iter()
        .map(|capsule| capsule.kind.0.as_str())
        .collect::<BTreeSet<_>>();
    for expected in [
        "core.user",
        "core.creature",
        "finance.wallet",
        "core.store_membership",
        "core.user_metadata",
        "guest.legacy_kv",
    ] {
        assert!(kinds.contains(expected), "export lacks {expected}");
    }

    let state = MemoryState::default();
    let source = LegacySource(Mutex::new(Vec::new()));
    source.set(&initial);
    let service = StorageMigrationService {
        state: &state,
        source: &source,
        target: &repository,
        clock: &Clock,
    };
    service
        .create(&StorageMigration::plan("e2e", 1, 60_000, true).unwrap())
        .unwrap();

    // Steps 3-4: canonical export, then bounded, resumable import.
    let export = CanonicalCapsuleExport::build(
        CapsuleExportHeader {
            format_version: 1,
            source_provider: "legacy-rocksdb-v1".to_owned(),
            source_snapshot_id: "e2e-snapshot".to_owned(),
            transform_manifest_digest: vec![1; 32],
            created_at_micros: 1_000,
        },
        initial.clone(),
    )
    .unwrap();
    let digest: [u8; 32] = export.trailer.stream_digest.clone().try_into().unwrap();
    service
        .record_export("e2e", digest, export.trailer.record_count)
        .unwrap();
    let mut sink = Sink {
        repository: &repository,
        guest: Vec::new(),
    };
    let mut checkpoint = None;
    loop {
        let report = import_canonical(&export, checkpoint.as_ref(), &mut sink, 2).unwrap();
        let done = report.checkpoint.next_sequence == export.trailer.record_count;
        checkpoint = Some(report.checkpoint);
        if done {
            break;
        }
    }
    // Replaying the whole stream is idempotent.
    let guest_count = initial
        .iter()
        .filter(|capsule| capsule.kind.0 == "guest.legacy_kv")
        .count() as u64;
    let replay = import_canonical(&export, None, &mut sink, 1_000).unwrap();
    assert_eq!(
        replay.already_present,
        export.trailer.record_count - guest_count
    );

    // Guest pairs land only in the owning creature's isolated database.
    let owner = aseman_storage_legacy::deterministic_legacy_capsule_id("Creature", b"1@global");
    // A fresh generation gives every run its own guest database (names derive from it).
    let run_generation = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros()
            % 1_000_000_000,
    )
    .unwrap()
        + 1;
    let provisioner = PostgresGuestProvisioner::new(&admin_uri, PROXY_ROLE).unwrap();
    let guest_db = provisioner
        .enable(&provisioner.provision(owner, run_generation).unwrap())
        .unwrap();
    let mut proxy = Config::from_str(&admin_uri).unwrap();
    proxy.user(PROXY_ROLE).password(PROXY_PASSWORD);
    let pools = GuestPoolRouter::from_config(proxy, PROXY_ROLE, 2, 2).unwrap();
    let guest_capsules = sink
        .guest
        .iter()
        .map(|capsule| (capsule.id.0, capsule.clone()))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect::<Vec<_>>();
    let guest_import = pools.import_legacy_kv(&guest_db, &guest_capsules).unwrap();
    assert_eq!(guest_import.inserted, 1);
    assert_eq!(
        pools
            .import_legacy_kv(&guest_db, &guest_capsules)
            .unwrap()
            .already_present,
        1
    );
    service
        .record_import("e2e", digest, export.trailer.record_count)
        .unwrap();

    // Step 5: semantic verification of every non-guest record.
    service.verify("e2e").unwrap();

    // Steps 6-7: the legacy side changes while delta capture runs.
    service.start_delta_capture("e2e").unwrap();
    let changed = dependency_order(transform(
        legacy_snapshot(&public_key, "updated bio"),
        2_000,
    ));
    source.set(&changed);
    assert!(
        service.apply_delta("e2e").is_err(),
        "an unapplied delta must block"
    );
    let migration = state.load("e2e").unwrap().unwrap();
    // The failed delta attempt left the phase unchanged; plan and apply the delta.
    assert_eq!(migration.phase, MigrationPhase::CapturingDelta);
    let non_guest = changed
        .iter()
        .filter(|capsule| capsule.kind.0 != "guest.legacy_kv")
        .cloned()
        .collect::<Vec<_>>();
    let writes = plan_delta(&non_guest, &repository.snapshot_all().unwrap(), 3_000).unwrap();
    assert_eq!(writes.len(), 1, "only the metadata document changed");
    for write in &writes {
        repository
            .put_fenced(
                &write.capsule,
                write.expected_revision,
                Some(migration.active_generation),
            )
            .unwrap();
    }
    service.apply_delta("e2e").unwrap();

    // Step 8: fenced cutover; late writes routed under the old generation are refused.
    let cut = service.cutover("e2e").unwrap();
    assert_eq!(cut.authority, Authority::Target);
    repository.raise_fence(cut.active_generation).unwrap();
    let stale = &writes[0];
    let mut late = stale.capsule.clone();
    late.revision += 1;
    late.previous_integrity = Some(stale.capsule.integrity_hash.clone());
    let late = late.seal().unwrap();
    assert!(
        repository
            .put_fenced(
                &late,
                Some(stale.capsule.revision),
                Some(cut.source_generation)
            )
            .is_err()
    );

    // Step 9: rollback stays available while reverse replication is intact.
    let rolled = service.rollback("e2e").unwrap();
    assert_eq!(rolled.authority, Authority::Source);
    assert!(rolled.active_generation > cut.active_generation);

    drop(repository);
    pools.retire(&guest_db).ok();
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)"))
        .unwrap();
}
