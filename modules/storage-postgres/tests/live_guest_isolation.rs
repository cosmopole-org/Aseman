use aseman_contracts::guest::{
    GuestBindingStatus, GuestColumnDefinition, GuestDefaultValue, GuestFieldType,
    GuestIndexDefinition, GuestSchemaCommand, GuestSchemaMutation,
};
use aseman_storage_postgres::guest::{
    GuestPoolRouter, GuestPostgresError, GuestSchemaManager, PostgresGuestProvisioner,
};
use postgres::{Client, Config, NoTls};
use std::str::FromStr;
use uuid::Uuid;

const PROXY_ROLE: &str = "aseman_guest_proxy_test";
const ATTACKER_ROLE: &str = "aseman_guest_attacker_test";
const PROXY_PASSWORD: &str = "proxy-test-password";
const ATTACKER_PASSWORD: &str = "attacker-test-password";

#[test]
fn live_guest_databases_isolate_roles_catalogs_pools_and_multi_table_ddl() {
    let Some(connection_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url
    else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping guest isolation test");
        return;
    };
    let mut admin = Client::connect(&connection_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!(
            "DO $$ BEGIN CREATE ROLE {PROXY_ROLE} LOGIN NOINHERIT; \
             EXCEPTION WHEN duplicate_object THEN NULL; END $$; \
             ALTER ROLE {PROXY_ROLE} LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
             NOREPLICATION NOBYPASSRLS PASSWORD '{PROXY_PASSWORD}'; \
             DO $$ BEGIN CREATE ROLE {ATTACKER_ROLE} LOGIN NOINHERIT; \
             EXCEPTION WHEN duplicate_object THEN NULL; END $$; \
             ALTER ROLE {ATTACKER_ROLE} LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
             NOREPLICATION NOBYPASSRLS PASSWORD '{ATTACKER_PASSWORD}';"
        ))
        .unwrap();

    let provisioner = PostgresGuestProvisioner::new(&connection_uri, PROXY_ROLE).unwrap();
    let creature_a = *Uuid::now_v7().as_bytes();
    let creature_b = *Uuid::now_v7().as_bytes();
    let disabled_a = provisioner.provision(creature_a, 1).unwrap();
    let disabled_b = provisioner.provision(creature_b, 1).unwrap();
    assert_eq!(disabled_a.binding().status, GuestBindingStatus::Disabled);
    assert_eq!(disabled_b.binding().status, GuestBindingStatus::Disabled);
    assert_ne!(
        disabled_a.binding().database_name,
        disabled_b.binding().database_name
    );
    assert_ne!(
        disabled_a.binding().role_name,
        disabled_b.binding().role_name
    );

    let role_row = admin
        .query_one(
            "SELECT rolcanlogin, rolsuper, rolcreatedb, rolcreaterole \
             FROM pg_roles WHERE rolname = $1",
            &[&disabled_a.binding().role_name],
        )
        .unwrap();
    assert!(!role_row.get::<_, bool>(0));
    assert!(!role_row.get::<_, bool>(1));
    assert!(!role_row.get::<_, bool>(2));
    assert!(!role_row.get::<_, bool>(3));

    let mut active_a = provisioner.enable(&disabled_a).unwrap();
    let mut active_b = provisioner.enable(&disabled_b).unwrap();
    let mut proxy_config = Config::from_str(&connection_uri).unwrap();
    proxy_config.user(PROXY_ROLE).password(PROXY_PASSWORD);
    let pools = GuestPoolRouter::from_config(proxy_config.clone(), PROXY_ROLE, 2, 2).unwrap();
    let schemas = GuestSchemaManager::default();
    let mutations: Vec<GuestSchemaMutation> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/capsule/guest/fixtures/valid-multi-table.json"
    )))
    .unwrap();
    for mutation in &mutations {
        active_a = apply_schema(&schemas, &pools, &active_a, mutation.clone());
    }
    active_b = apply_schema(
        &schemas,
        &pools,
        &active_b,
        GuestSchemaMutation::CreateTable {
            definition: aseman_contracts::guest::GuestTableDefinition {
                name: "private_notes".to_owned(),
                columns: std::collections::BTreeMap::from([(
                    "body".to_owned(),
                    GuestColumnDefinition {
                        field_type: GuestFieldType::Text,
                        required: true,
                        default: Some(GuestDefaultValue::Text(String::new())),
                    },
                )]),
                primary_key: Vec::new(),
                indexes: Vec::new(),
                foreign_keys: Vec::new(),
            },
        },
    );

    active_a = apply_schema(
        &schemas,
        &pools,
        &active_a,
        GuestSchemaMutation::AddColumn {
            table: "customers".to_owned(),
            name: "nickname".to_owned(),
            definition: GuestColumnDefinition {
                field_type: GuestFieldType::Text,
                required: true,
                default: Some(GuestDefaultValue::Text("anonymous".to_owned())),
            },
        },
    );
    let extra_index = GuestSchemaMutation::CreateIndex {
        table: "customers".to_owned(),
        definition: GuestIndexDefinition {
            name: "customers_nickname".to_owned(),
            columns: vec!["nickname".to_owned()],
            unique: false,
        },
    };
    active_a = apply_schema(&schemas, &pools, &active_a, extra_index);
    active_a = apply_schema(
        &schemas,
        &pools,
        &active_a,
        GuestSchemaMutation::DropIndex {
            table: "customers".to_owned(),
            name: "customers_nickname".to_owned(),
        },
    );

    let stale = schemas.apply(
        &pools,
        &active_a,
        &GuestSchemaCommand {
            expected_catalog_revision: active_a.binding().schema_catalog_revision - 1,
            mutation: GuestSchemaMutation::CreateTable {
                definition: aseman_contracts::guest::GuestTableDefinition {
                    name: "must_not_exist".to_owned(),
                    columns: std::collections::BTreeMap::from([(
                        "value".to_owned(),
                        GuestColumnDefinition {
                            field_type: GuestFieldType::Integer,
                            required: true,
                            default: Some(GuestDefaultValue::Integer(0)),
                        },
                    )]),
                    primary_key: Vec::new(),
                    indexes: Vec::new(),
                    foreign_keys: Vec::new(),
                },
            },
        },
    );
    assert!(matches!(stale, Err(GuestPostgresError::SchemaConflict)));

    pools
        .with_transaction(&active_a, |transaction| {
            let row = transaction
                .query_one(
                    "SELECT current_database()::text, current_user::text, \
                     to_regclass('aseman_guest.customers')::text, \
                     to_regclass('aseman_guest.orders')::text, \
                     to_regclass('aseman_guest.private_notes')::text",
                    &[],
                )
                .map_err(db_error)?;
            assert_eq!(row.get::<_, String>(0), active_a.binding().database_name);
            assert_eq!(row.get::<_, String>(1), active_a.binding().role_name);
            assert!(row.get::<_, Option<String>>(2).is_some());
            assert!(row.get::<_, Option<String>>(3).is_some());
            assert!(row.get::<_, Option<String>>(4).is_none());
            let forbidden = transaction.query_one(
                "SELECT revision FROM aseman_guard.schema_catalog WHERE singleton",
                &[],
            );
            assert!(forbidden.is_err());
            Ok(())
        })
        .unwrap_err();

    pools
        .with_transaction(&active_a, |transaction| {
            let row = transaction
                .query_one(
                    "SELECT to_regclass('aseman_guest.customers')::text, \
                     to_regclass('aseman_guest.must_not_exist')::text",
                    &[],
                )
                .map_err(db_error)?;
            assert!(row.get::<_, Option<String>>(0).is_some());
            assert!(row.get::<_, Option<String>>(1).is_none());
            Ok(())
        })
        .unwrap();

    let rollback = pools.with_transaction(&active_a, |transaction| {
        transaction
            .batch_execute("CREATE TEMP TABLE should_not_leak(value integer)")
            .map_err(db_error)?;
        Err::<(), _>(GuestPostgresError::Invalid("forced rollback".to_owned()))
    });
    assert!(rollback.is_err());
    pools
        .with_transaction(&active_a, |transaction| {
            let leaked: Option<String> = transaction
                .query_one("SELECT to_regclass('pg_temp.should_not_leak')::text", &[])
                .map_err(db_error)?
                .get(0);
            assert!(leaked.is_none());
            Ok(())
        })
        .unwrap();

    let contaminated = pools.with_transaction(&active_a, |transaction| {
        transaction
            .batch_execute(&format!(
                "SET LOCAL ROLE \"{}\"",
                active_b.binding().role_name
            ))
            .map_err(db_error)
    });
    assert!(matches!(
        contaminated,
        Err(GuestPostgresError::Contaminated)
    ));
    pools
        .with_transaction(&active_a, |transaction| {
            let role: String = transaction
                .query_one("SELECT current_user::text", &[])
                .map_err(db_error)?
                .get(0);
            assert_eq!(role, active_a.binding().role_name);
            Ok(())
        })
        .unwrap();

    let one_pool = GuestPoolRouter::from_config(proxy_config, PROXY_ROLE, 1, 1).unwrap();
    one_pool
        .with_transaction(&active_a, |_transaction| Ok(()))
        .unwrap();
    assert!(matches!(
        one_pool.with_transaction(&active_b, |_transaction| Ok(())),
        Err(GuestPostgresError::PoolCapacity)
    ));

    let mut attacker_config = Config::from_str(&connection_uri).unwrap();
    attacker_config
        .user(ATTACKER_ROLE)
        .password(ATTACKER_PASSWORD)
        .dbname(&active_a.binding().database_name);
    assert!(attacker_config.connect(NoTls).is_err());

    let disabled_a = provisioner.disable(&active_a).unwrap();
    let disabled_b = provisioner.disable(&active_b).unwrap();
    pools.retire(&disabled_a).unwrap();
    pools.retire(&disabled_b).unwrap();
    assert!(matches!(
        pools.with_transaction(&disabled_a, |_transaction| Ok(())),
        Err(GuestPostgresError::Inactive)
    ));
    drop(one_pool);
    drop(pools);

    cleanup(
        &mut admin,
        &[
            (
                disabled_a.binding().database_name.as_str(),
                disabled_a.binding().role_name.as_str(),
            ),
            (
                disabled_b.binding().database_name.as_str(),
                disabled_b.binding().role_name.as_str(),
            ),
        ],
    );
}

fn db_error(error: postgres::Error) -> GuestPostgresError {
    GuestPostgresError::Database(error.to_string())
}

fn apply_schema(
    schemas: &GuestSchemaManager,
    pools: &GuestPoolRouter,
    binding: &aseman_storage_postgres::guest::ProvisionedGuestDatabase,
    mutation: GuestSchemaMutation,
) -> aseman_storage_postgres::guest::ProvisionedGuestDatabase {
    schemas
        .apply(
            pools,
            binding,
            &GuestSchemaCommand {
                expected_catalog_revision: binding.binding().schema_catalog_revision,
                mutation,
            },
        )
        .unwrap()
}

fn cleanup(admin: &mut Client, databases: &[(&str, &str)]) {
    for (database, _) in databases {
        let _ = admin.query(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE datname = $1 AND pid <> pg_backend_pid()",
            &[database],
        );
        admin
            .batch_execute(&format!("DROP DATABASE IF EXISTS \"{database}\""))
            .unwrap();
    }
    for (_, role) in databases {
        admin
            .batch_execute(&format!("DROP ROLE IF EXISTS \"{role}\""))
            .unwrap();
    }
    admin
        .batch_execute(&format!(
            "DROP ROLE IF EXISTS {ATTACKER_ROLE}; DROP ROLE IF EXISTS {PROXY_ROLE};"
        ))
        .unwrap();
}
