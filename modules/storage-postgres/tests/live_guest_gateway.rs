//! A405 end to end on live PostgreSQL: the capsule catalog resolves workload -> program
//! -> creature -> binding, the gateway authorizes with the registry policy, and each
//! workload's operations land only in its own creature's database.

use aseman_application::guest::GuestGateway;
use aseman_application::identity::IdentityFailure;
use aseman_capsule_repositories::capability::CapsuleGrantStore;
use aseman_capsule_repositories::creature::CapsuleCreaturePorts;
use aseman_capsule_repositories::program::CapsuleProgramPorts;
use aseman_capsule_repositories::workload::CapsuleWorkloads;
use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleRelationship, CapsuleValue,
    OwnerScope, StorageClass,
};
use aseman_contracts::legacy_realtime::deterministic_legacy_capsule_id;
use aseman_domain::creature::CreatureRecord;
use aseman_domain::guest::{
    GUEST_DATA_ACTION, GuestKvOperation, GuestKvOutcome, LegacyKvNamespace,
};
use aseman_domain::identity::{Subject, SubjectKind};
use aseman_domain::program::ProgramRecord;
use aseman_domain::{BindingStatus, CreatureDatabaseBinding, CreatureId, Uuid, WorkloadId};
use aseman_policy_native::RegistryPolicy;
use aseman_ports::{
    ClockPort, CreatureDatabaseBindings, CreatureDirectory, GuestKv, ProgramDirectory,
    WorkloadRepository,
};
use aseman_storage_postgres::PostgresCapsuleRepository;
use aseman_storage_postgres::guest::{GuestPoolRouter, PostgresGuestKv, PostgresGuestProvisioner};
use postgres::{Client, Config, NoTls};
use rsa::pkcs8::{EncodePublicKey, LineEnding};
use std::str::FromStr;

const PROXY_ROLE: &str = "aseman_guest_gateway_proxy_test";
const PROXY_PASSWORD: &str = "gateway-proxy-test-password";

struct Clock;

impl ClockPort for Clock {
    fn unix_millis(&self) -> i64 {
        1_800_000_000_000
    }
}

fn public_key() -> String {
    rsa::RsaPublicKey::from(&rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap())
        .to_public_key_pem(LineEnding::LF)
        .unwrap()
}

fn workload_capsule(workload: Uuid, program: [u8; 16], creature: [u8; 16]) -> CapsuleEnvelope {
    let relationship = |name: &str, kind: &str, target: [u8; 16]| CapsuleRelationship {
        name: name.to_owned(),
        target_kind: CapsuleKind(kind.to_owned()),
        target_id: CapsuleId(target),
    };
    CapsuleEnvelope {
        encoding_version: 1,
        id: CapsuleId(*workload.as_bytes()),
        kind: CapsuleKind("core.workload".to_owned()),
        storage_class: StorageClass::Core,
        owner_scope: OwnerScope::Creature(creature),
        schema_version: 1,
        revision: 1,
        created_at_micros: 1,
        updated_at_micros: 1,
        previous_integrity: None,
        integrity_hash: CapsuleDigest {
            algorithm: "sha2-256".to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: false,
        relationships: vec![
            relationship("program", "core.program", program),
            relationship("creature", "core.creature", creature),
        ],
        body: Some(CapsuleValue::Object(
            [
                ("workload_name", CapsuleValue::Text(workload.to_string())),
                ("runtime", CapsuleValue::Text("wasm".to_owned())),
                ("desired_state", CapsuleValue::Text("running".to_owned())),
                ("desired_generation", CapsuleValue::Integer(1)),
            ]
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
        )),
    }
    .seal()
    .unwrap()
}

#[test]
fn live_guest_gateway_resolves_and_isolates_each_workload() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping guest gateway test");
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
    let database = format!("aseman_gateway_{}", Uuid::now_v7().simple());
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);
    let repository = PostgresCapsuleRepository::from_client(config.connect(NoTls).unwrap());
    repository.migrate().unwrap();

    // Two human creatures, each with a program and a running workload.
    let creatures = CapsuleCreaturePorts {
        repository: &repository,
        currency: "USD",
        scale: 2,
    };
    let programs = CapsuleProgramPorts {
        repository: &repository,
    };
    let catalog = CapsuleWorkloads {
        repository: &repository,
    };
    let provisioner = PostgresGuestProvisioner::new(&admin_uri, PROXY_ROLE).unwrap();
    // Guest databases are derived from the creature, so each run uses fresh creatures.
    let run = Uuid::now_v7().simple().to_string();
    let mut placed = Vec::new();
    for name in ["alice", "bob", "carol"] {
        let legacy_id = format!("{name}-{run}@gateway");
        creatures
            .create(&CreatureRecord {
                id: legacy_id.clone(),
                creature_type: "human".to_owned(),
                username: format!("{name}-{run}@gateway.name"),
                public_key: public_key(),
                chain_id: "main".to_owned(),
                subchain_id: "main".to_owned(),
                owner_id: aseman_domain::creature::HUMAN_OWNER.to_owned(),
            })
            .unwrap();
        let creature = deterministic_legacy_capsule_id("Creature", legacy_id.as_bytes());
        programs
            .create_program(&ProgramRecord {
                id: format!("{name}-{run}-program@gateway"),
                machine_id: legacy_id.clone(),
                runtime: "wasm".to_owned(),
                path: "/".to_owned(),
                comment: String::new(),
            })
            .unwrap();
        let program = deterministic_legacy_capsule_id(
            "Program",
            format!("{name}-{run}-program@gateway").as_bytes(),
        );
        placed.push((creature, program));
    }
    // Workload records: creation, uniqueness, the creature chain, compare-and-set.
    aseman_ports::conformance::workload_repository(
        &catalog,
        CreatureId::from_uuid(Uuid::from_bytes(placed[2].0)),
        aseman_domain::ProgramId::from_uuid(Uuid::from_bytes(placed[2].1)),
        aseman_domain::ProgramId::from_uuid(Uuid::from_bytes(placed[1].1)),
    );
    // The binding catalog on a creature without a binding.
    aseman_ports::conformance::creature_database_bindings(
        &catalog,
        CreatureId::from_uuid(Uuid::from_bytes(placed[2].0)),
    );

    let mut proxy_config = Config::from_str(&admin_uri).unwrap();
    proxy_config.user(PROXY_ROLE).password(PROXY_PASSWORD);
    let kv =
        PostgresGuestKv::new(GuestPoolRouter::from_config(proxy_config, PROXY_ROLE, 4, 2).unwrap());
    let mut bindings = Vec::new();
    let mut workloads = Vec::new();
    for (index, (creature, program)) in placed.iter().take(2).enumerate() {
        let provisioned = provisioner
            .enable(&provisioner.provision(*creature, 1).unwrap())
            .unwrap();
        let mut binding = CreatureDatabaseBinding::new(
            CreatureId::from_uuid(Uuid::from_bytes(*creature)),
            provisioned.binding().provider_id.clone(),
            provisioned.binding().database_name.clone(),
            provisioned.binding().role_name.clone(),
        )
        .unwrap();
        binding.status = BindingStatus::Active;
        catalog.record_binding(&binding).unwrap();
        bindings.push(binding);
        let workload = Uuid::from_u128(0x9a7e_0000 + index as u128);
        repository
            .put(&workload_capsule(workload, *program, *creature), None)
            .unwrap();
        workloads.push(Subject {
            kind: SubjectKind::Workload,
            id: workload,
        });
    }

    // The KV semantics on a real creature database.
    aseman_ports::conformance::guest_kv(&kv, &bindings[0]);

    let policy = RegistryPolicy::compiled("live").unwrap();
    let grants = CapsuleGrantStore {
        repository: &repository,
    };
    let gateway = GuestGateway {
        workloads: &catalog,
        bindings: &catalog,
        policy: &policy,
        grants: &grants,
        clock: &Clock,
        kv: &kv,
    };
    let key = |key: &str| key.to_owned();
    for (workload, value) in workloads.iter().zip(["alice's", "bob's"]) {
        assert_eq!(
            gateway.execute(
                *workload,
                GUEST_DATA_ACTION,
                &GuestKvOperation::Put {
                    namespace: LegacyKvNamespace::DbOp,
                    key: key("secret"),
                    value: value.to_owned(),
                },
            ),
            Ok(GuestKvOutcome::Written)
        );
    }
    for (workload, value) in workloads.iter().zip(["alice's", "bob's"]) {
        assert_eq!(
            gateway.execute(
                *workload,
                GUEST_DATA_ACTION,
                &GuestKvOperation::Get {
                    namespace: LegacyKvNamespace::DbOp,
                    key: key("secret"),
                },
            ),
            Ok(GuestKvOutcome::Value {
                value: Some(value.to_owned())
            })
        );
    }
    // The resolved chain is the trusted one.
    let resolved = catalog
        .get_desired(WorkloadId::from_uuid(workloads[1].id))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.creature_id.as_uuid().as_bytes(), &placed[1].0);

    // A catalog record pointing at another creature's database cannot route.
    let tampered = CreatureDatabaseBinding {
        creature_id: bindings[1].creature_id,
        ..bindings[0].clone()
    };
    assert!(
        kv.execute(
            &tampered,
            &GuestKvOperation::Get {
                namespace: LegacyKvNamespace::DbOp,
                key: key("secret"),
            },
        )
        .is_err()
    );
    // A workload whose program belongs to another creature fails closed.
    let crossed = Uuid::from_u128(0x9a7e_00ff);
    repository
        .put(&workload_capsule(crossed, placed[0].1, placed[1].0), None)
        .unwrap();
    assert!(catalog.get_desired(WorkloadId::from_uuid(crossed)).is_err());
    assert!(matches!(
        gateway.execute(
            Subject {
                kind: SubjectKind::Workload,
                id: crossed,
            },
            GUEST_DATA_ACTION,
            &GuestKvOperation::Get {
                namespace: LegacyKvNamespace::DbOp,
                key: key("secret"),
            },
        ),
        Err(IdentityFailure::Unavailable(_))
    ));

    drop(repository);
    admin
        .batch_execute(&format!("DROP DATABASE {database}"))
        .unwrap();
}
