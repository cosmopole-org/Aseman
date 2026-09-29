//! The node's state ports pass the port conformance suites on the storage module
//! (ADR 0036). The suites build on one another — creatures own programs, programs
//! own entities and alarms, stores need their creator — so they run in order in one
//! transaction, as the live PostgreSQL suite does.

use aseman_domain::creature::CreatureRecord;
use aseman_domain::store::StoreRecord;
use aseman_ports::conformance;
use aseman_ports::{CreatureDirectory, StoreDirectory, VmResourceStores};

use crate::state::creature_ports::CreaturePorts;
use crate::state::entity_ports::EntityPorts;
use crate::state::gateway_ports::GatewayPorts;
use crate::state::program_ports::ProgramPorts;
use crate::state::store_ports::{MembershipPorts, StorePorts};

pub(crate) fn public_keys() -> [String; 5] {
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    [0, 1, 2, 3, 4].map(|_| {
        rsa::RsaPublicKey::from(&rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap())
            .to_public_key_pem(LineEnding::LF)
            .unwrap()
    })
}

/// Record human creatures `ids` (each needs a distinct key).
pub(crate) fn seed_humans(trx: &crate::storage::Trx, ids: &[&str]) {
    let creatures = CreaturePorts { trx };
    for (id, key) in ids.iter().zip(public_keys()) {
        creatures
            .create(&CreatureRecord {
                id: (*id).to_owned(),
                creature_type: "human".to_owned(),
                username: format!("{}.name", id.replace('@', "-")),
                public_key: key,
                chain_id: "main".to_owned(),
                subchain_id: "main".to_owned(),
                owner_id: aseman_domain::creature::HUMAN_OWNER.to_owned(),
            })
            .unwrap();
    }
}

#[test]
fn state_ports_pass_the_conformance_suites_in_dependency_order() {
    let trx = crate::storage::test_trx();
    let creatures = CreaturePorts { trx: &trx };
    let keys = public_keys();
    conformance::creature_directory(&creatures, &creatures, [&keys[0], &keys[1], &keys[2]]);
    conformance::creature_metadata(&creatures, &creatures, &keys[4]);
    conformance::creature_types(&creatures);
    creatures
        .create(&CreatureRecord {
            id: "7@conformance".to_owned(),
            creature_type: "human".to_owned(),
            username: "carol@conformance".to_owned(),
            public_key: keys[3].clone(),
            chain_id: "main".to_owned(),
            subchain_id: "main".to_owned(),
            owner_id: "free".to_owned(),
        })
        .unwrap();

    let programs = ProgramPorts { trx: &trx };
    conformance::program_directory(&programs, ["1@conformance", "7@conformance"]);
    conformance::program_metadata(&programs, "12@conformance");

    let stores = StorePorts { trx: &trx };
    conformance::store_directory(&stores, &stores, "1@conformance");
    conformance::store_access(
        &MembershipPorts { trx: &trx },
        "s-1@conformance",
        ["1@conformance", "7@conformance"],
    );
    conformance::gateway_routes(
        &GatewayPorts { trx: &trx },
        "1@conformance",
        "alice",
        "12@conformance",
    );

    stores
        .create_store(
            &StoreRecord {
                id: "store-1".to_owned(),
                persistent_history: true,
                member_count: 1,
                ..Default::default()
            },
            "1@conformance",
        )
        .unwrap();
    conformance::program_alarms(&programs, "12@conformance", "store-1");
    let entities = EntityPorts { trx: &trx };
    conformance::entity_directory(&entities, "12@conformance");
    conformance::vm_resource_stores(&programs, ["1@conformance", "7@conformance"]);
    programs
        .put_resource_store("vs-conformance", "s", "1@conformance", "{}")
        .unwrap();
    conformance::vm_resource_entities(&entities, "vs-conformance");
}
