//! Live: the routed guest data paths against a real creature database.

use super::*;
use aseman_capsule_repositories::creature::CapsuleCreaturePorts;
use aseman_domain::creature::CreatureRecord;
use aseman_domain::CreatureDatabaseBinding;
use aseman_ports::CreatureDirectory;
use aseman_storage_postgres::guest::{GuestPoolRouter, PostgresGuestProvisioner};
use postgres::{Client, Config, NoTls};
use rsa::pkcs8::{EncodePublicKey, LineEnding};
use std::str::FromStr;

const PROXY_ROLE: &str = "aseman_guest_data_proxy_test";
const PROXY_PASSWORD: &str = "guest-data-proxy-test-password";

#[test]
fn live_guest_data_routes_to_the_creatures_database() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping guest data routing test");
        return;
    };
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!(
            "DO $$ BEGIN CREATE ROLE {PROXY_ROLE} LOGIN NOINHERIT; \
             EXCEPTION WHEN duplicate_object THEN NULL; END $$; \
             ALTER ROLE {PROXY_ROLE} LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
             NOREPLICATION NOBYPASSRLS PASSWORD '{PROXY_PASSWORD}';"
        ))
        .unwrap();
    let database = format!("aseman_guest_data_{}", Uuid::now_v7().simple());
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);
    let catalog = PostgresCapsuleRepository::from_client(config.connect(NoTls).unwrap());
    catalog.migrate().unwrap();

    let run = Uuid::now_v7().simple().to_string();
    let creature = format!("gd-{run}@global");
    let key = rsa::RsaPublicKey::from(
        &rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap(),
    )
    .to_public_key_pem(LineEnding::LF)
    .unwrap();
    CapsuleCreaturePorts {
        repository: &catalog,
        currency: "USD",
        scale: 2,
    }
    .create(&CreatureRecord {
        id: creature.clone(),
        creature_type: "human".to_owned(),
        username: format!("gd-{run}@global.name"),
        public_key: key,
        chain_id: "main".to_owned(),
        subchain_id: "main".to_owned(),
        owner_id: aseman_domain::creature::HUMAN_OWNER.to_owned(),
    })
    .unwrap();
    let creature_capsule = deterministic_legacy_capsule_id("Creature", creature.as_bytes());
    let provisioner = PostgresGuestProvisioner::new(&admin_uri, PROXY_ROLE).unwrap();
    let provisioned = provisioner
        .enable(&provisioner.provision(creature_capsule, 1).unwrap())
        .unwrap();
    let mut binding = CreatureDatabaseBinding::new(
        CreatureId::from_uuid(Uuid::from_bytes(creature_capsule)),
        provisioned.binding().provider_id.clone(),
        provisioned.binding().database_name.clone(),
        provisioned.binding().role_name.clone(),
    )
    .unwrap();
    binding.status = BindingStatus::Active;
    CapsuleWorkloads {
        repository: &catalog,
    }
    .record_binding(&binding)
    .unwrap();
    let mut proxy = Config::from_str(&admin_uri).unwrap();
    proxy.user(PROXY_ROLE).password(PROXY_PASSWORD);
    let routing = GuestRouting {
        kv: PostgresGuestKv::new(GuestPoolRouter::from_config(proxy, PROXY_ROLE, 2, 2).unwrap()),
        catalog,
    };

    // Documents and links (ADR 0028), in the legacy response shapes.
    let state = |op: &str, input: Value| state_with(&routing, &creature, op, &input).unwrap();
    assert_eq!(
        state(
            "putJson",
            json!({"key": "counter", "path": "doc", "data": {"n": 1}})
        ),
        json!({"ok": true})
    );
    assert_eq!(
        state("getJson", json!({"key": "counter", "path": "doc"})),
        json!({"ok": true, "data": {"n": 1}})
    );
    assert_eq!(
        state("getByPrefix", json!({"prefix": ""})),
        json!({"ok": true, "data": ["counter::doc", "counter::doc.n"]})
    );
    state("delKey", json!({"key": "counter"}));
    assert_eq!(
        state("getByPrefix", json!({"prefix": ""})),
        json!({"ok": true, "data": []})
    );
    assert!(state_with(&routing, &creature, "getJson", &json!({})).is_err());

    // `dbOp` pairs in both namespaces, and `getLink` over the creature's own pairs.
    let db = |namespace, op: &str, key: &str, value: &str, prefix: &str| {
        db_op_with(&routing, &creature, namespace, op, key, value, prefix).unwrap()
    };
    let dbop = LegacyKvNamespace::DbOp;
    let applet = LegacyKvNamespace::AppletDb;
    db(dbop, "put", "profile", "alice", "");
    db(applet, "put", "p1::counter", "3", "");
    assert_eq!(
        db(dbop, "get", "profile", "", ""),
        json!({"data": "alice"}).to_string()
    );
    assert_eq!(
        db(applet, "getByPrefix", "", "", "p1::"),
        json!({"data": ["3"]}).to_string()
    );
    assert_eq!(
        state("getLink", json!({"key": "profile"})),
        json!({"ok": true, "value": "alice"})
    );
    db(dbop, "del", "profile", "", "");
    assert_eq!(
        db(dbop, "get", "profile", "", ""),
        json!({"data": ""}).to_string()
    );

    // A creature without an active binding is refused, never served from legacy.
    assert_eq!(
        db_op_with(&routing, "nobody@global", dbop, "get", "k", "", ""),
        Err("the creature's guest database is not active".to_owned())
    );

    drop(routing);
    admin
        .batch_execute(&format!("DROP DATABASE {database}"))
        .unwrap();
}

#[test]
fn runtime_keys_name_their_creature() {
    assert_eq!(
        split_runtime_key("8@global::profile"),
        Some(("8@global", "profile"))
    );
    assert_eq!(split_runtime_key("ModalProvisioning::vm-1"), None);
    assert_eq!(split_runtime_key("plain"), None);
}
