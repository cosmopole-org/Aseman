//! The PostgreSQL provider's cluster mode against real shards (ADR 0033).
//!
//! Two databases on one server act as two shards; PostgreSQL's two-phase commit spans
//! databases exactly as it spans servers. Needs `ASEMAN_TEST_POSTGRES_SHARDS_URL`, an
//! administrative URL of a server with `max_prepared_transactions > 0`; skips without.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aseman_capsule::CapsuleStore;
use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery, CapsuleValue,
    DIGEST_ALGORITHM, ENCODING_VERSION, OwnerScope, ProviderCapabilities, QueryError, QuerySort,
    SortDirection, StorageClass,
};
use aseman_storage_conformance::{StorageProviderHarness, query_error, reference_core_user_suite};
use aseman_storage_postgres::PostgresCapsuleRepository;
use aseman_storage_postgres::shard::{
    ShardMap, ShardSpec, ShardedUnitOfWorkFactory, UnitOfWorkFactory, shard_of,
};
use aseman_storage_postgres::unit_of_work::PostgresUnitOfWorkFactory;
use postgres::{Client, NoTls};

fn retarget(url: &str, database: &str) -> String {
    let scheme = url.find("://").unwrap() + 3;
    let slash = url[scheme..]
        .find('/')
        .map_or(url.len(), |index| scheme + index);
    format!("{}/{database}", &url[..slash])
}

struct Cluster {
    admin: String,
    shards: Vec<String>,
    factory: Arc<ShardedUnitOfWorkFactory>,
}

impl Drop for Cluster {
    fn drop(&mut self) {
        if let Ok(mut admin) = Client::connect(&self.admin, NoTls) {
            for shard in &self.shards {
                let _ =
                    admin.batch_execute(&format!("DROP DATABASE IF EXISTS {shard} WITH (FORCE)"));
            }
        }
    }
}

fn cluster(admin: &str) -> Cluster {
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let names: Vec<String> = ["a", "b"]
        .iter()
        .map(|shard| format!("aseman_shard_{shard}_{}", &suffix[suffix.len() - 12..]))
        .collect();
    let mut client = Client::connect(admin, NoTls).unwrap();
    for name in &names {
        client
            .batch_execute(&format!("CREATE DATABASE {name}"))
            .unwrap();
    }
    let map = ShardMap {
        version: 1,
        home: "a".to_owned(),
        read_from_replicas: false,
        shards: vec![
            ShardSpec {
                name: "a".to_owned(),
                primary: retarget(admin, &names[0]),
                replicas: Vec::new(),
                guest_proxy: None,
            },
            ShardSpec {
                name: "b".to_owned(),
                primary: retarget(admin, &names[1]),
                replicas: Vec::new(),
                guest_proxy: None,
            },
        ],
    };
    Cluster {
        admin: admin.to_owned(),
        factory: Arc::new(ShardedUnitOfWorkFactory::connect(map, 4, None).unwrap()),
        shards: names,
    }
}

fn count(url: &str, sql: &str) -> i64 {
    Client::connect(url, NoTls)
        .unwrap()
        .query_one(sql, &[])
        .unwrap()
        .get(0)
}

fn unavailable(error: impl ToString) -> QueryError {
    query_error(
        aseman_contracts::capsule::QueryErrorCode::Unavailable,
        error.to_string(),
    )
}

/// The conformance kit's error codes from a store error (test harness only: the
/// store carries the provider's message, whose wording names its class).
fn store(error: aseman_capsule::CapsuleStoreError) -> QueryError {
    use aseman_contracts::capsule::QueryErrorCode;
    match error {
        aseman_capsule::CapsuleStoreError::Conflict => {
            query_error(QueryErrorCode::RevisionConflict, "revision conflict")
        }
        aseman_capsule::CapsuleStoreError::Failed(message) => {
            let lower = message.to_ascii_lowercase();
            let code = if lower.contains("unsupported") || lower.contains("not advertised") {
                QueryErrorCode::UnsupportedCapability
            } else if lower.contains("invalid") || lower.contains("outside provider bounds") {
                QueryErrorCode::InvalidQuery
            } else {
                QueryErrorCode::Unavailable
            };
            query_error(code, message)
        }
    }
}

/// The conformance kit through the sharded store: every call is its own unit.
struct ShardedHarness<'a> {
    cluster: &'a Cluster,
}

impl StorageProviderHarness for ShardedHarness<'_> {
    fn reset(&mut self) -> Result<(), QueryError> {
        for shard in &self.cluster.shards {
            Client::connect(&retarget(&self.cluster.admin, shard), NoTls)
                .and_then(|mut client| {
                    client.batch_execute("TRUNCATE TABLE aseman_core.users CASCADE")
                })
                .map_err(unavailable)?;
        }
        Ok(())
    }

    fn capabilities(&self) -> ProviderCapabilities {
        PostgresCapsuleRepository::capabilities()
    }

    fn put(&mut self, capsule: CapsuleEnvelope, expected: Option<u64>) -> Result<(), QueryError> {
        let unit = self.cluster.factory.begin().map_err(unavailable)?;
        match unit.put(&capsule, expected) {
            Ok(()) => unit.commit().map_err(unavailable),
            Err(error) => {
                let _ = unit.rollback();
                Err(store(error))
            }
        }
    }

    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> Result<Option<CapsuleEnvelope>, QueryError> {
        let unit = self.cluster.factory.begin().map_err(unavailable)?;
        let found = unit.get(kind, id).map_err(store);
        let _ = unit.rollback();
        found
    }

    fn query(&self, query: &CapsuleQuery) -> Result<Vec<CapsuleEnvelope>, QueryError> {
        let unit = self.cluster.factory.begin().map_err(unavailable)?;
        let found = unit.query(query).map_err(store);
        let _ = unit.rollback();
        found
    }
}

fn build_log(id: CapsuleId, workload: &str, observed_at: i64) -> CapsuleEnvelope {
    CapsuleEnvelope {
        encoding_version: ENCODING_VERSION,
        id,
        kind: CapsuleKind("telemetry.build_log".to_owned()),
        storage_class: StorageClass::Telemetry,
        owner_scope: OwnerScope::Global,
        schema_version: 1,
        revision: 1,
        created_at_micros: observed_at,
        updated_at_micros: observed_at,
        previous_integrity: None,
        integrity_hash: CapsuleDigest {
            algorithm: DIGEST_ALGORITHM.to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: false,
        relationships: Vec::new(),
        body: Some(CapsuleValue::Object(BTreeMap::from([
            (
                "legacy_id".to_owned(),
                CapsuleValue::Text(format!("log-{observed_at}")),
            ),
            (
                "build_id".to_owned(),
                CapsuleValue::Text("build-1".to_owned()),
            ),
            (
                "machine_id".to_owned(),
                CapsuleValue::Text("machine-1".to_owned()),
            ),
            (
                "workload_id".to_owned(),
                CapsuleValue::Text(workload.to_owned()),
            ),
            (
                "log_type".to_owned(),
                CapsuleValue::Text("stdout".to_owned()),
            ),
            (
                "message".to_owned(),
                CapsuleValue::Text(format!("line {observed_at}")),
            ),
            (
                "observed_at_micros".to_owned(),
                CapsuleValue::Integer(observed_at),
            ),
        ]))),
    }
    .seal()
    .unwrap()
}

#[test]
fn a_sharded_cluster_keeps_every_guarantee_of_one_database() {
    let Some(admin) = aseman_config::IntegrationTestConfig::from_process().postgres_shards_url
    else {
        eprintln!("ASEMAN_TEST_POSTGRES_SHARDS_URL is absent; skipping the sharding suite");
        return;
    };
    let cluster = cluster(&admin);
    let shard_urls: Vec<String> = cluster
        .shards
        .iter()
        .map(|name| retarget(&admin, name))
        .collect();

    // 1. The whole storage conformance suite passes through the sharded store.
    let report = reference_core_user_suite()
        .run(&mut ShardedHarness { cluster: &cluster })
        .unwrap();
    assert!(!report.passed_cases.is_empty());

    // 2. A reference kind lands on every shard, atomically, through two-phase commit.
    let users = "SELECT count(*) FROM aseman_core.users";
    let before: Vec<i64> = shard_urls.iter().map(|url| count(url, users)).collect();
    let mut kit = reference_core_user_suite();
    kit.initial.id = CapsuleId(*uuid::Uuid::now_v7().as_bytes());
    kit.initial.integrity_hash.bytes = vec![0; 32];
    let user = kit.initial.clone().seal().unwrap();
    let unit = cluster.factory.begin().unwrap();
    unit.put(&user, None).unwrap();
    unit.commit().unwrap();
    for (url, before) in shard_urls.iter().zip(&before) {
        assert_eq!(
            count(url, users),
            before + 1,
            "reference rows replicate to {url}"
        );
        assert_eq!(count(url, "SELECT count(*) FROM pg_prepared_xacts"), 0);
    }

    // 3. Distributed rows spread by hash; a fanned-out query merges them in order.
    let ids: Vec<CapsuleId> = (0..24)
        .map(|_| CapsuleId(*uuid::Uuid::now_v7().as_bytes()))
        .collect();
    let unit = cluster.factory.begin().unwrap();
    for (index, id) in ids.iter().enumerate() {
        unit.put(
            &build_log(id.clone(), "workload-9", 1_000 + index as i64),
            None,
        )
        .unwrap();
    }
    unit.commit().unwrap();
    let logs = "SELECT count(*) FROM aseman_telemetry.build_logs";
    let per_shard: Vec<i64> = shard_urls.iter().map(|url| count(url, logs)).collect();
    assert_eq!(per_shard.iter().sum::<i64>(), 24);
    for (shard, rows) in per_shard.iter().enumerate() {
        let expected = ids.iter().filter(|id| shard_of(id, 2) == shard).count();
        assert_eq!(
            *rows, expected as i64,
            "shard {shard} holds exactly its hash share"
        );
        assert!(*rows > 0, "both shards take part");
    }
    let query = CapsuleQuery {
        kind: CapsuleKind("telemetry.build_log".to_owned()),
        predicate: None,
        projection: Default::default(),
        sort: vec![QuerySort {
            field: "observed_at_micros".to_owned(),
            direction: SortDirection::Descending,
        }],
        aggregates: Vec::new(),
        traversals: Vec::new(),
        limit: 5,
        cursor: None,
    };
    let unit = cluster.factory.begin().unwrap();
    let newest: Vec<i64> = unit
        .query(&query)
        .unwrap()
        .into_iter()
        .map(|capsule| capsule.created_at_micros)
        .collect();
    let _ = unit.rollback();
    assert_eq!(newest, vec![1_023, 1_022, 1_021, 1_020, 1_019]);

    // 4. A rolled-back unit leaves nothing on any shard.
    let mut discarded = reference_core_user_suite().initial;
    discarded.id = CapsuleId(*uuid::Uuid::now_v7().as_bytes());
    discarded.body = Some(CapsuleValue::Object(BTreeMap::from([
        (
            "username".to_owned(),
            CapsuleValue::Text("discarded".to_owned()),
        ),
        (
            "email".to_owned(),
            CapsuleValue::Text("discarded@example.invalid".to_owned()),
        ),
        ("public_key".to_owned(), CapsuleValue::Bytes(vec![3; 32])),
        ("status".to_owned(), CapsuleValue::Text("active".to_owned())),
    ])));
    let discarded = discarded.seal().unwrap();
    let unit = cluster.factory.begin().unwrap();
    unit.put(&discarded, None).unwrap();
    unit.rollback().unwrap();
    for (url, before) in shard_urls.iter().zip(&before) {
        assert_eq!(count(url, users), before + 1);
    }

    // 5. Recovery after a coordinator crash between the phases: a decided transaction
    //    is committed, an undecided one rolled back.
    for (decided, username) in [(true, "decided"), (false, "undecided")] {
        let gid = format!("aseman-test{}", uuid::Uuid::now_v7().simple());
        let mut capsule = reference_core_user_suite().initial;
        capsule.id = CapsuleId(*uuid::Uuid::now_v7().as_bytes());
        capsule.body = Some(CapsuleValue::Object(BTreeMap::from([
            (
                "username".to_owned(),
                CapsuleValue::Text(username.to_owned()),
            ),
            (
                "email".to_owned(),
                CapsuleValue::Text(format!("{username}@example.invalid")),
            ),
            (
                "public_key".to_owned(),
                CapsuleValue::Bytes(vec![if decided { 4 } else { 5 }; 32]),
            ),
            ("status".to_owned(), CapsuleValue::Text("active".to_owned())),
        ])));
        let capsule = capsule.seal().unwrap();
        for (shard, url) in shard_urls.iter().enumerate() {
            let unit = PostgresUnitOfWorkFactory::connect(url, 1, None)
                .unwrap()
                .begin()
                .unwrap();
            unit.put(&capsule, None).unwrap();
            // Phase one only: the coordinator "crashes" before phase two.
            std::mem::forget(unit.prepare(&format!("{gid}-{shard}")).unwrap());
        }
        if decided {
            Client::connect(&shard_urls[0], NoTls)
                .unwrap()
                .execute(
                    "INSERT INTO aseman_core.shard_commit_decisions (gid) VALUES ($1)",
                    &[&gid],
                )
                .unwrap();
        }
        assert_eq!(
            cluster.factory.recover_older_than(Duration::ZERO).unwrap(),
            2
        );
        let expected = i64::from(decided);
        for url in &shard_urls {
            assert_eq!(count(url, "SELECT count(*) FROM pg_prepared_xacts"), 0);
            assert_eq!(
                count(
                    url,
                    &format!(
                        "SELECT count(*) FROM aseman_core.users WHERE username = '{username}'"
                    )
                ),
                expected
            );
        }
    }
}
