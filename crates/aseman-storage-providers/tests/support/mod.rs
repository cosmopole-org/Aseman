//! The providers every storage-module port adapter is tested on (ADR 0038): the
//! in-memory provider, an embedded RocksDB store, and a fresh PostgreSQL database when
//! `ASEMAN_TEST_POSTGRES_URL` names a server.

#![allow(dead_code)]

use aseman_storage::{ProviderSettings, Storage};

/// A store under test; dropping it removes what it created.
pub struct TestStore {
    pub name: &'static str,
    pub storage: Storage,
    cleanup: Option<Box<dyn FnOnce()>>,
}

impl Drop for TestStore {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

fn unique(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7().simple())
}

pub fn memory() -> TestStore {
    TestStore {
        name: "memory",
        storage: Storage::new(
            aseman_storage::memory::MemoryProvider::new(),
            aseman_storage::schema::Schema::catalog().unwrap(),
        ),
        cleanup: None,
    }
}

pub fn rocksdb() -> TestStore {
    let root = std::env::temp_dir().join(unique("aseman_ports_rocksdb"));
    let storage = Storage::open(
        &aseman_storage_providers::registry(),
        "rocksdb",
        &ProviderSettings::embedded(&root).unwrap(),
    )
    .unwrap();
    TestStore {
        name: "rocksdb",
        storage,
        cleanup: Some(Box::new(move || {
            let _ = std::fs::remove_dir_all(root);
        })),
    }
}

/// An empty database on the test server: its URL, and a guard that drops it.
pub struct FreshDatabase {
    pub url: String,
    admin: postgres::Client,
    name: String,
}

impl FreshDatabase {
    /// `None` when no test server is configured.
    pub fn create() -> Option<Self> {
        let admin_url = aseman_config::IntegrationTestConfig::from_process().postgres_url?;
        let name = unique("aseman_ports");
        let mut admin = postgres::Client::connect(&admin_url, postgres::NoTls).unwrap();
        admin
            .batch_execute(&format!("CREATE DATABASE {name}"))
            .unwrap();
        let (base, query) = admin_url
            .split_once('?')
            .map_or((admin_url.as_str(), None), |(base, query)| {
                (base, Some(query))
            });
        let server = &base[..base.rfind('/').expect("a database URL has a path")];
        let url = match query {
            Some(query) => format!("{server}/{name}?{query}"),
            None => format!("{server}/{name}"),
        };
        Some(Self { url, admin, name })
    }

    pub fn client(&self) -> postgres::Client {
        postgres::Client::connect(&self.url, postgres::NoTls).unwrap()
    }
}

impl Drop for FreshDatabase {
    fn drop(&mut self) {
        let _ = self
            .admin
            .batch_execute(&format!("DROP DATABASE {} WITH (FORCE)", self.name));
    }
}

/// The PostgreSQL provider on a fresh database, or `None` when none is configured.
pub fn postgres(max_connections: u32) -> Option<TestStore> {
    let database = FreshDatabase::create()?;
    let storage =
        aseman_storage_providers::open_database(database.url.clone(), max_connections).unwrap();
    Some(TestStore {
        name: "postgres",
        storage,
        cleanup: Some(Box::new(move || drop(database))),
    })
}

/// Every provider available here.
pub fn stores() -> Vec<TestStore> {
    let mut stores = vec![memory(), rocksdb()];
    stores.extend(postgres(8));
    stores
}
