//! Three replicas of one RocksDB storage cluster in one process, over real loopback
//! HTTP: the seed bootstraps, the others join through `/cluster/add-peer`, and writes
//! through any replica's [`ReplicatedKvStore`] become visible on every replica in log
//! order, including deletes and multi-key batches.

use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use std::sync::OnceLock;

use aseman_admin_http::MutualTls;
use aseman_admin_http::testing::TestAuthority;

use super::config::ClusterConfig;
use super::*;

fn free_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().to_string()
}

struct Replica {
    store: ReplicatedKvStore,
    local: Arc<RocksDbKvStore>,
    addr: String,
}

/// The one CA every replica of a test cluster, and its administrator, is issued by.
fn authority() -> &'static TestAuthority {
    static AUTHORITY: OnceLock<TestAuthority> = OnceLock::new();
    AUTHORITY.get_or_init(|| TestAuthority::new("aseman-test-cluster"))
}

fn replica(root: &Path, id: u64, seed: bool) -> Replica {
    let dir = root.join(format!("replica-{id}"));
    std::fs::create_dir_all(&dir).unwrap();
    let tls = authority().identity(&dir.join("tls"), &format!("replica-{id}"));
    let local = Arc::new(RocksDbKvStore::open_default(&dir.join("kv")).unwrap());
    let addr = free_addr();
    let cfg = ClusterConfig {
        enabled: true,
        bootstrap: seed,
        node_id: id,
        node_name: format!("replica-{id}"),
        listen_addr: addr.clone(),
        advertise_addr: addr.clone(),
        auth_token: "cluster-test-token".to_owned(),
        tls,
        ..ClusterConfig::default()
    };
    let svc = start_service(
        local.clone(),
        cfg,
        dir.join("cluster.json"),
        &RocksDbTuning::default(),
        None,
    )
    .unwrap();
    Replica {
        store: ReplicatedKvStore {
            local: local.clone(),
            cluster: Some(svc),
        },
        local,
        addr,
    }
}

fn admin_post(addr: &str, path: &str, body: serde_json::Value) {
    let directory =
        std::env::temp_dir().join(format!("aseman-cluster-admin-{}", std::process::id()));
    let admin = MutualTls::load(&authority().identity(&directory, "admin")).unwrap();
    let response = admin
        .blocking_http_client(Duration::from_secs(30))
        .unwrap()
        .post(format!("https://{addr}{path}"))
        .header("x-aseman-cluster-token", "cluster-test-token")
        .json(&body)
        .timeout(Duration::from_secs(30))
        .send()
        .unwrap();
    assert!(
        response.status().is_success(),
        "{path}: {}",
        response.text().unwrap_or_default()
    );
}

fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn put(key: &str, value: &str) -> LegacyKvWrite {
    LegacyKvWrite::Put {
        key: key.as_bytes().to_vec(),
        value: value.as_bytes().to_vec(),
    }
}

#[test]
fn writes_through_any_replica_reach_every_replica_in_log_order() {
    let root = std::env::temp_dir().join(format!("aseman-rocksdb-cluster-{}", uuid_like()));
    let seed = replica(&root, 1, true);
    let seed_cluster = seed.store.cluster().unwrap().clone();
    eventually("the seed to lead", || {
        seed_cluster.raft().metrics().borrow().current_leader == Some(1)
    });
    let second = replica(&root, 2, false);
    let third = replica(&root, 3, false);
    for joining in [&second, &third] {
        let id = joining.store.cluster().unwrap().node_id;
        admin_post(
            &seed.addr,
            "/cluster/add-peer",
            serde_json::json!({"id": id, "addr": joining.addr, "voter": true}),
        );
    }
    let replicas = [&seed, &second, &third];
    eventually("every replica to know the leader", || {
        replicas.iter().all(|replica| {
            replica
                .store
                .cluster()
                .unwrap()
                .raft()
                .metrics()
                .borrow()
                .current_leader
                == Some(1)
        })
    });

    // A follower's write is forwarded, committed, and readable at once on that
    // follower (read-your-writes), then on every replica.
    second
        .store
        .write_batch(&[put("obj::User::1::name", "ada"), put("link::a::b", "1")])
        .unwrap();
    assert_eq!(
        second.store.get(b"obj::User::1::name").unwrap(),
        Some(b"ada".to_vec())
    );
    // The leader's write and a delete.
    seed.store
        .write_batch(&[
            put("obj::User::2::name", "grace"),
            LegacyKvWrite::Delete {
                key: b"link::a::b".to_vec(),
            },
        ])
        .unwrap();
    third
        .store
        .write_batch(&[put("obj::User::1::name", "ada l.")])
        .unwrap();

    for replica in replicas {
        eventually("replication", || {
            replica.local.get(b"obj::User::2::name").unwrap() == Some(b"grace".to_vec())
                && replica.local.get(b"obj::User::1::name").unwrap() == Some(b"ada l.".to_vec())
        });
        assert_eq!(replica.local.get(b"link::a::b").unwrap(), None);
        assert_eq!(
            replica.local.scan_prefix(b"obj::User::").unwrap().len(),
            2,
            "every replica holds exactly the committed keys"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Three joined replicas whose seed leads.
fn joined_cluster(root: &Path) -> [Replica; 3] {
    let seed = replica(root, 1, true);
    let seed_cluster = seed.store.cluster().unwrap().clone();
    eventually("the seed to lead", || {
        seed_cluster.raft().metrics().borrow().current_leader == Some(1)
    });
    let second = replica(root, 2, false);
    let third = replica(root, 3, false);
    for joining in [&second, &third] {
        let id = joining.store.cluster().unwrap().node_id;
        admin_post(
            &seed.addr,
            "/cluster/add-peer",
            serde_json::json!({"id": id, "addr": joining.addr, "voter": true}),
        );
    }
    let replicas = [seed, second, third];
    eventually("every replica to know the leader", || {
        replicas.iter().all(|replica| {
            replica
                .store
                .cluster()
                .unwrap()
                .raft()
                .metrics()
                .borrow()
                .current_leader
                == Some(1)
        })
    });
    replicas
}

#[test]
fn capsules_replicate_and_compare_and_set_holds_across_replicas() {
    use crate::capsule_store::RocksDbCapsuleStore;
    use aseman_capsule::{CapsuleStore, CapsuleStoreError};
    use aseman_contracts::capsule::{
        CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleValue, DIGEST_ALGORITHM,
        ENCODING_VERSION, OwnerScope, StorageClass,
    };

    let root = std::env::temp_dir().join(format!("aseman-rocksdb-capsules-{}", uuid_like()));
    let [seed, second, third] = joined_cluster(&root);
    let locals = [
        seed.local.clone(),
        second.local.clone(),
        third.local.clone(),
    ];
    let stores = [seed, second, third].map(|replica| {
        let kv: Arc<dyn LegacyKvStore> = Arc::new(replica.store);
        RocksDbCapsuleStore::open(kv, true).unwrap()
    });
    let user = |revision: u64, status: &str, previous: Option<&CapsuleEnvelope>| {
        CapsuleEnvelope {
            encoding_version: ENCODING_VERSION,
            id: CapsuleId([4; 16]),
            kind: CapsuleKind("core.user".to_owned()),
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Global,
            schema_version: 1,
            revision,
            created_at_micros: 1,
            updated_at_micros: i64::try_from(revision).unwrap(),
            previous_integrity: previous.map(|previous| previous.integrity_hash.clone()),
            integrity_hash: CapsuleDigest {
                algorithm: DIGEST_ALGORITHM.to_owned(),
                bytes: vec![0; 32],
            },
            tombstone: false,
            relationships: Vec::new(),
            body: Some(CapsuleValue::Object(BTreeMap::from([
                ("username".to_owned(), CapsuleValue::Text("ada".to_owned())),
                ("public_key".to_owned(), CapsuleValue::Bytes(vec![4; 8])),
                ("status".to_owned(), CapsuleValue::Text(status.to_owned())),
            ]))),
        }
        .seal()
        .unwrap()
    };

    // A follower's write is readable on that follower at once and on every replica.
    let first = user(1, "active", None);
    stores[1].put(&first, None).unwrap();
    assert_eq!(
        stores[1].get(&first.kind, &first.id).unwrap(),
        Some(first.clone())
    );
    let away = user(2, "away", Some(&first));
    stores[0].put(&away, Some(1)).unwrap();
    for store in &stores {
        eventually("replication", || {
            store.get(&away.kind, &away.id).unwrap() == Some(away.clone())
        });
    }

    // A competing revision 2 from another replica loses: the state machine checks the
    // precondition in log order on every replica.
    let busy = user(2, "busy", Some(&first));
    assert_eq!(
        stores[2].put(&busy, Some(1)),
        Err(CapsuleStoreError::Conflict)
    );
    // The unique username holds cluster-wide.
    let mut impostor = user(1, "active", None);
    impostor.id = CapsuleId([5; 16]);
    let impostor = impostor.seal().unwrap();
    assert_eq!(
        stores[2].put(&impostor, None),
        Err(CapsuleStoreError::Conflict)
    );
    for (store, local) in stores.iter().zip(&locals) {
        assert_eq!(store.get(&away.kind, &away.id).unwrap(), Some(away.clone()));
        assert_eq!(store.get(&impostor.kind, &impostor.id).unwrap(), None);
        assert!(
            !local
                .scan_prefix(b"aseman/capsule/row/")
                .unwrap()
                .is_empty()
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_single_host_store_writes_locally() {
    let root = std::env::temp_dir().join(format!("aseman-rocksdb-local-{}", uuid_like()));
    let local = Arc::new(RocksDbKvStore::open_default(&root).unwrap());
    let store = ReplicatedKvStore::local(local);
    assert!(store.cluster().is_none());
    store.write_batch(&[put("k", "v")]).unwrap();
    assert_eq!(store.get(b"k").unwrap(), Some(b"v".to_vec()));
    let _ = std::fs::remove_dir_all(&root);
}

fn uuid_like() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}
