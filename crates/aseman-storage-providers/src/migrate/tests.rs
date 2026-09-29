//! Migration on embedded RocksDB: a legacy store converts into models in place, and
//! a model store copies exactly into another provider instance.

use super::*;
use aseman_domain::guest::{GuestKvOperation, GuestKvOutcome};
use aseman_ports::consensus_log::ConsensusLogWrite;
use aseman_storage::client::core::creature;
use aseman_storage_rocksdb::{LegacyKvStore, LegacyKvWrite, RocksDbKvStore};

fn temp_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "aseman-migrate-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn rocksdb(root: &Path, legacy_store: Option<PathBuf>) -> Storage {
    let mut settings = ProviderSettings::embedded(root).unwrap();
    settings.legacy_store = legacy_store;
    open(aseman_storage_rocksdb::model_store::NAME, &settings).unwrap()
}

fn put(pairs: &[(&str, Vec<u8>)], path: &Path) {
    let store = RocksDbKvStore::open_default(path).unwrap();
    store
        .write_batch(
            &pairs
                .iter()
                .map(|(key, value)| LegacyKvWrite::Put {
                    key: key.as_bytes().to_vec(),
                    value: value.clone(),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
}

/// A legacy base: one human creature with a guest pair, a program, a store, a
/// finance counter, and the id counters.
fn legacy_base(path: &Path) {
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    let key = rsa::RsaPublicKey::from(
        &rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap(),
    )
    .to_public_key_pem(LineEnding::LF)
    .unwrap();
    let mut pairs: Vec<(String, Vec<u8>)> = Vec::new();
    for (column, value) in [
        ("|", vec![1]),
        ("type", b"human".to_vec()),
        ("username", b"alice@node".to_vec()),
        ("publicKey", key.as_bytes().to_vec()),
        ("chainId", b"main".to_vec()),
        ("subchainId", b"main".to_vec()),
        ("ownerId", b"free".to_vec()),
        ("balance", 1_000_i64.to_le_bytes().to_vec()),
    ] {
        pairs.push((format!("obj::Creature::1@global::{column}"), value));
    }
    for (column, value) in [
        ("|", vec![1]),
        ("id", b"2@global".to_vec()),
        ("machineId", b"1@global".to_vec()),
        ("runtime", b"wasm".to_vec()),
        ("path", b"/programs/two".to_vec()),
    ] {
        pairs.push((format!("obj::Program::2@global::{column}"), value));
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
        pairs.push((format!("obj::Store::3@global::{column}"), value));
    }
    for (key, value) in [
        ("index::Creature::username::id::alice@node", "1@global"),
        ("link::machinePrograms::1@global::2@global", "true"),
        ("link::creatorof::1@global::3@global", "true"),
        ("link::onaccess::3@global::1@global", "read,signal,manage"),
        ("link::hasaccess::1@global::3@global", "true"),
        ("link::1@global::counter", "5"),
        ("link::FinanceDebt::1@global", "7"),
    ] {
        pairs.push((key.to_owned(), value.as_bytes().to_vec()));
    }
    pairs.push(("globalIdCounter".to_owned(), 10_i64.to_be_bytes().to_vec()));
    put(
        &pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect::<Vec<_>>(),
        path,
    );
}

fn options(root: &Path) -> LegacyOptions {
    LegacyOptions {
        storage_root: root.to_owned(),
        currency: "ASE".to_owned(),
        scale: 0,
        local_origins: BTreeSet::from(["global".to_owned()]),
        file_artifacts: BTreeMap::new(),
        signal_log: None,
    }
}

#[test]
fn a_legacy_rocksdb_store_converts_into_models_in_place() {
    let root = temp_root("legacy");
    let base = root.join("base");
    legacy_base(&base);
    // A Hashgraph log from before ADR 0036.
    let legacy_log =
        RocksDbConsensusLogStorage::new(&root, aseman_config::RocksDbTuning::default());
    legacy_log
        .open("chains/main/shard-main/rocksdb_db", false)
        .unwrap()
        .write(&[ConsensusLogWrite::Put {
            key: b"block_000000001".to_vec(),
            value: b"b1".to_vec(),
        }])
        .unwrap();

    let storage = rocksdb(&root, Some(base.clone()));
    assert!(storage.provider().legacy_layout().unwrap().is_some());
    let mut report = Report::default();
    convert_legacy(
        &LegacySource::RocksDb(base.clone()),
        &options(&root),
        &storage,
        &mut report,
    )
    .unwrap();
    relocate_legacy_logs(
        aseman_storage_rocksdb::model_store::NAME,
        storage.provider().consensus_logs().as_ref(),
        &root,
        storage.provider().consensus_logs().as_ref(),
        aseman_config::RocksDbTuning::default(),
        &mut report,
    )
    .unwrap();
    storage.provider().retire_legacy_layout().unwrap();
    assert_eq!(storage.provider().legacy_layout().unwrap(), None);
    assert!(!base.exists(), "the legacy base is set aside");

    let trx = storage.begin(Mode::ReadOnly).unwrap();
    assert_eq!(trx.creature().count(None).unwrap(), 1);
    assert!(
        trx.creature()
            .find_unique(creature::by_username("alice@node"))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        trx.counter()
            .find_unique(counter::by_key("global"))
            .unwrap()
            .unwrap()
            .value,
        10
    );
    let ledger = StorageFinanceLedger { trx: &trx };
    assert_eq!(ledger.counter(WalletCounter::Debt, "1@global").unwrap(), 7);
    drop(trx);
    let guest = guest_kv::StorageGuestKv::new(storage.clone());
    let alice = CreatureId::from_uuid(Uuid::from_bytes(Id::for_key("Creature", "1@global").0));
    assert_eq!(
        guest
            .execute_for(
                alice,
                &GuestKvOperation::Get {
                    namespace: aseman_domain::guest::LegacyKvNamespace::DbOp,
                    key: "counter".to_owned(),
                }
            )
            .unwrap(),
        GuestKvOutcome::Value {
            value: Some("5".to_owned())
        }
    );
    assert_eq!(report.guest_pairs, 1);
    assert_eq!(report.consensus_logs["chains/main/shard-main"], 1);
    let logs = storage.provider().consensus_logs();
    assert_eq!(logs.names().unwrap(), ["chains/main/shard-main"]);
    assert_eq!(
        logs.open("chains/main/shard-main", false)
            .unwrap()
            .get(b"block_000000001")
            .unwrap(),
        Some(b"b1".to_vec())
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_model_store_copies_exactly_into_an_empty_provider() {
    let (from, to) = (temp_root("from"), temp_root("to"));
    let source = rocksdb(&from, None);
    let trx = source.begin(Mode::ReadWrite).unwrap();
    for name in ["global", "local"] {
        trx.counter()
            .create(counter::Create {
                key: name.to_owned(),
                value: 3,
            })
            .unwrap();
    }
    trx.counter().delete(counter::by_key("local")).unwrap();
    trx.marker()
        .create(marker::Create {
            key: "m".to_owned(),
            value: "v".to_owned(),
        })
        .unwrap();
    trx.commit().unwrap();
    source
        .provider()
        .consensus_logs()
        .open("chains/main/shard-main", false)
        .unwrap()
        .write(&[ConsensusLogWrite::Put {
            key: vec![0, 1],
            value: vec![2],
        }])
        .unwrap();

    let target = rocksdb(&to, None);
    ensure_empty(&target).unwrap();
    let mut report = Report::default();
    copy_models(&source, &target, &mut report).unwrap();
    copy_consensus_logs(
        source.provider().consensus_logs().as_ref(),
        target.provider().consensus_logs().as_ref(),
        &mut report,
    )
    .unwrap();
    // The tombstone travels with its record.
    assert_eq!(report.models["core.counter"], 2);
    assert_eq!(report.models["core.marker"], 1);
    assert_eq!(report.consensus_logs["chains/main/shard-main"], 1);
    let trx = target.begin(Mode::ReadOnly).unwrap();
    assert_eq!(
        trx.counter()
            .find_unique(counter::by_key("global"))
            .unwrap()
            .unwrap()
            .value,
        3
    );
    assert!(
        trx.counter()
            .find_unique(counter::by_key("local"))
            .unwrap()
            .is_none()
    );
    drop(trx);
    assert!(ensure_empty(&target).is_err(), "a second copy is refused");
    let _ = std::fs::remove_dir_all(from);
    let _ = std::fs::remove_dir_all(to);
}

#[test]
fn absolute_log_names_become_relative() {
    let root = Path::new("/var/lib/aseman");
    assert_eq!(
        relative_log_name("/var/lib/aseman/chains/main/shard-main/rocksdb_db", root),
        "chains/main/shard-main"
    );
    assert_eq!(
        relative_log_name("/elsewhere/chains/x/y/rocksdb_db", root),
        "elsewhere/chains/x/y"
    );
}

/// A fresh PostgreSQL database for `label`, and its URL.
fn fresh_postgres(admin: &str, label: &str) -> String {
    let database = format!(
        "aseman_migrate_{label}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut client = postgres::Client::connect(admin, postgres::NoTls).unwrap();
    client
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let (base, _) = admin.rsplit_once('/').unwrap();
    format!("{base}/{database}")
}

#[test]
fn live_legacy_rocksdb_converts_into_postgres_and_copies_back() {
    let Some(admin) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping live migration test");
        return;
    };
    let root = temp_root("live");
    let base = root.join("base");
    legacy_base(&base);
    let mut settings = ProviderSettings::embedded(&root).unwrap();
    let url = fresh_postgres(&admin, "target");
    settings.database_url = Some(url.clone());
    // The legacy signal history PostgreSQL served (ASEMAN_SIGNAL_LOG_PROVIDER=postgres).
    postgres::Client::connect(&url, postgres::NoTls)
        .unwrap()
        .batch_execute(
            "CREATE SCHEMA aseman_legacy_log;
             CREATE TABLE aseman_legacy_log.signals(id text PRIMARY KEY, store_id text NOT NULL,
               user_id text NOT NULL, data text NOT NULL, tags text, time bigint NOT NULL,
               edited boolean NOT NULL DEFAULT false);
             INSERT INTO aseman_legacy_log.signals VALUES
               ('sig-1', '3@global', '1@global', 'hello', '|kind=message|', 1700000000000, false);",
        )
        .unwrap();
    let postgres = open(aseman_storage_postgres::plugin::NAME, &settings).unwrap();
    ensure_empty(&postgres).unwrap();
    let mut report = Report::default();
    let mut options = options(&root);
    options.signal_log = Some(LegacySignalLogSource::Postgres { url });
    convert_legacy(
        &LegacySource::RocksDb(base),
        &options,
        &postgres,
        &mut report,
    )
    .unwrap();
    assert_eq!(report.models["realtime.event"], 1);
    postgres
        .provider()
        .consensus_logs()
        .open("chains/main/shard-main", false)
        .unwrap()
        .write(&[ConsensusLogWrite::Put {
            key: b"round_000000001".to_vec(),
            value: b"r1".to_vec(),
        }])
        .unwrap();

    let back = temp_root("back");
    let rocksdb = rocksdb(&back, None);
    let mut copied = Report::default();
    copy_models(&postgres, &rocksdb, &mut copied).unwrap();
    copy_consensus_logs(
        postgres.provider().consensus_logs().as_ref(),
        rocksdb.provider().consensus_logs().as_ref(),
        &mut copied,
    )
    .unwrap();
    // Every converted model copied, plus what the bridge wrote.
    for (model, count) in &report.models {
        assert!(copied.models[model] >= *count, "{model} copied short");
    }
    for storage in [&postgres, &rocksdb] {
        let trx = storage.begin(Mode::ReadOnly).unwrap();
        assert_eq!(trx.creature().count(None).unwrap(), 1);
        assert_eq!(
            trx.counter()
                .find_unique(counter::by_key("global"))
                .unwrap()
                .unwrap()
                .value,
            10
        );
        let ledger = StorageFinanceLedger { trx: &trx };
        assert_eq!(ledger.counter(WalletCounter::Debt, "1@global").unwrap(), 7);
        // The store's signal history reads through the store ports.
        let history = aseman_ports::SignalLog::history(
            &aseman_capsule::store::CapsuleStorePorts {
                repository: &trx,
                stream_policy: &aseman_contracts::signals::SignalStreamPolicy::for_store,
            },
            "3@global",
            &aseman_domain::signal_tags::LogQuery {
                count: 10,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            history
                .iter()
                .map(|signal| signal.id.as_str())
                .collect::<Vec<_>>(),
            ["sig-1"]
        );
    }
    assert_eq!(copied.consensus_logs["chains/main/shard-main"], 1);
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(back);
}

fn config(root: &Path, extra: &[(&str, String)]) -> AsemanConfig {
    let mut values = BTreeMap::from([
        ("ASEMAN_NODE_ID".to_owned(), "drill".to_owned()),
        (
            "ASEMAN_NODE_PRIVATE_KEY_SECRET".to_owned(),
            root.join("node.key").display().to_string(),
        ),
        (
            "ASEMAN_CORE_STORAGE_PROVIDER".to_owned(),
            "rocksdb".to_owned(),
        ),
        (
            "ASEMAN_STORAGE_ROOT_PATH".to_owned(),
            root.display().to_string(),
        ),
        (
            "ASEMAN_BASE_DB_PATH".to_owned(),
            root.join("base").display().to_string(),
        ),
    ]);
    for (key, value) in extra {
        values.insert((*key).to_owned(), value.clone());
    }
    AsemanConfig::from_map(&values).unwrap()
}

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// The command as `asemanctl storage migrate` runs it: in place on a legacy RocksDB
/// store, then (live) into PostgreSQL.
#[test]
fn the_migrate_command_converts_in_place_then_switches_provider() {
    let root = temp_root("drill");
    legacy_base(&root.join("base"));
    let config = config(&root, &[]);

    let plan = command(&config, &args(&["--dry-run", "--signal-log", "none"])).unwrap();
    assert!(
        plan.contains("dry run") && plan.contains("legacy key/value"),
        "{plan}"
    );
    assert!(root.join("base").exists(), "a dry run changes nothing");

    let done = command(&config, &args(&["--signal-log", "none"])).unwrap();
    assert!(done.contains("core.creature: 1 record(s)"), "{done}");
    assert!(!root.join("base").exists(), "the legacy base is retired");
    let again = command(&config, &args(&["--signal-log", "none"])).unwrap();
    assert!(again.contains("nothing to migrate"), "{again}");

    if let Some(admin) = aseman_config::IntegrationTestConfig::from_process().postgres_url {
        let secret = root.join("database-url");
        std::fs::write(&secret, fresh_postgres(&admin, "drill")).unwrap();
        let copied = command(
            &config,
            &args(&[
                "--to",
                "postgres",
                "--database-url-secret",
                &secret.display().to_string(),
                "--signal-log",
                "none",
            ]),
        )
        .unwrap();
        assert!(copied.contains("core.creature: 1 record(s)"), "{copied}");
        assert!(
            copied.contains("ASEMAN_CORE_STORAGE_PROVIDER=postgres"),
            "{copied}"
        );
        // The target is not empty any more: a second copy is refused.
        assert!(
            command(
                &config,
                &args(&[
                    "--to",
                    "postgres",
                    "--database-url-secret",
                    &secret.display().to_string(),
                    "--signal-log",
                    "none",
                ]),
            )
            .is_err()
        );
    }
    let _ = std::fs::remove_dir_all(root);
}
