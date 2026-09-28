//! Three replicas of one RocksDB storage cluster in one process, over real loopback
//! HTTP: the seed bootstraps, the others join through `/cluster/add-peer`, and writes
//! through any replica's [`ReplicatedKvStore`] become visible on every replica in log
//! order, including deletes and multi-key batches.

use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

fn replica(root: &Path, id: u64, seed: bool) -> Replica {
    let dir = root.join(format!("replica-{id}"));
    std::fs::create_dir_all(&dir).unwrap();
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
        ..ClusterConfig::default()
    };
    let svc = start_service(local.clone(), cfg, dir.join("cluster.json"), None).unwrap();
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
    let response = reqwest::blocking::Client::new()
        .post(format!("http://{addr}{path}"))
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
