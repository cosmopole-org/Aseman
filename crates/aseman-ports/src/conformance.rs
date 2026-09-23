//! Behavioral suites every adapter of a port must pass (feature `conformance`).
//!
//! The legacy adapters and the capsule adapters run the same suite, so a use case
//! behaves identically before and after cutover.

pub mod coordination;
pub mod federation;
pub mod realtime;
pub mod vmm;

use crate::{CreatureBalances, CreatureDirectory, PortError};
use aseman_domain::creature::CreatureRecord;

/// Exercises [`CreatureDirectory`] and [`CreatureBalances`] on an empty directory.
///
/// `keys` are three distinct RSA SPKI public keys in standard LF PEM spelling; the
/// capsule encoding normalizes other spellings of a key to this one.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn creature_directory(
    directory: &dyn CreatureDirectory,
    balances: &dyn CreatureBalances,
    keys: [&str; 3],
) {
    let alice = CreatureRecord {
        id: "1@conformance".to_owned(),
        creature_type: "human".to_owned(),
        username: "alice@conformance".to_owned(),
        public_key: keys[0].to_owned(),
        chain_id: "main".to_owned(),
        subchain_id: "main".to_owned(),
        owner_id: "free".to_owned(),
    };
    let bob = CreatureRecord {
        id: "2@conformance".to_owned(),
        creature_type: "machine".to_owned(),
        username: "bob@conformance".to_owned(),
        public_key: keys[1].to_owned(),
        chain_id: "main".to_owned(),
        subchain_id: "side".to_owned(),
        owner_id: alice.id.clone(),
    };
    assert_eq!(directory.creature(&alice.id), Ok(None));
    assert_eq!(balances.balance(&alice.id), Err(PortError::NotFound));
    directory.create(&alice).unwrap();
    balances.open(&alice.id, 10).unwrap();
    directory.create(&bob).unwrap();
    balances.open(&bob.id, 5).unwrap();
    assert_eq!(balances.open(&bob.id, 1), Err(PortError::Conflict));
    assert_eq!(directory.creature(&alice.id), Ok(Some(alice.clone())));
    assert_eq!(directory.creature(&bob.id), Ok(Some(bob.clone())));
    assert_eq!(balances.balance(&alice.id), Ok(10));
    assert_eq!(balances.balance(&bob.id), Ok(5));
    assert_eq!(
        directory.creature_id_by_username("bob@conformance"),
        Ok(Some(bob.id.clone()))
    );
    assert_eq!(
        directory.creature_id_by_username("nobody@conformance"),
        Ok(None)
    );

    // A taken username is refused, and nothing is written.
    let impostor = CreatureRecord {
        id: "3@conformance".to_owned(),
        public_key: keys[2].to_owned(),
        ..bob.clone()
    };
    assert_eq!(directory.create(&impostor), Err(PortError::Conflict));
    assert_eq!(directory.creature(&impostor.id), Ok(None));

    // Identity order, type filter, and the legacy window.
    let ids = |records: Vec<CreatureRecord>| {
        records
            .into_iter()
            .map(|record| record.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(directory.creatures(None, 0, None).unwrap()),
        [alice.id.clone(), bob.id.clone()]
    );
    assert_eq!(
        ids(directory.creatures(Some("machine"), 0, None).unwrap()),
        std::slice::from_ref(&bob.id)
    );
    assert_eq!(
        ids(directory.creatures(None, 1, Some(1)).unwrap()),
        std::slice::from_ref(&bob.id)
    );
    assert!(directory.creatures(None, 0, Some(-1)).unwrap().is_empty());

    // Fragment search walks usernames in order.
    assert_eq!(
        directory.find_by_username_fragment("@conformance"),
        Ok(Some(alice.clone()))
    );
    assert_eq!(
        directory.find_by_username_fragment("bo"),
        Ok(Some(bob.clone()))
    );
    assert_eq!(directory.find_by_username_fragment("zzz"), Ok(None));

    // A rename moves the username; the old one is free again.
    let renamed = CreatureRecord {
        username: "robert@conformance".to_owned(),
        public_key: keys[2].to_owned(),
        creature_type: "agent".to_owned(),
        ..bob.clone()
    };
    directory.update(&renamed).unwrap();
    assert_eq!(directory.creature(&bob.id), Ok(Some(renamed.clone())));
    assert_eq!(
        directory.creature_id_by_username("bob@conformance"),
        Ok(None)
    );
    assert_eq!(
        directory.creature_id_by_username("robert@conformance"),
        Ok(Some(bob.id.clone()))
    );
    assert_eq!(balances.balance(&bob.id), Ok(5));
    let clash = CreatureRecord {
        username: alice.username.clone(),
        ..renamed.clone()
    };
    assert_eq!(directory.update(&clash), Err(PortError::Conflict));
    assert_eq!(directory.creature(&bob.id), Ok(Some(renamed.clone())));
    assert_eq!(directory.update(&impostor), Err(PortError::NotFound));

    balances.set_balance(&bob.id, 42).unwrap();
    assert_eq!(balances.balance(&bob.id), Ok(42));
    assert_eq!(balances.balance(&alice.id), Ok(10));

    directory.delete(&bob.id).unwrap();
    balances.close(&bob.id).unwrap();
    balances.close(&bob.id).unwrap();
    assert_eq!(directory.creature(&bob.id), Ok(None));
    assert_eq!(balances.balance(&bob.id), Err(PortError::NotFound));
    assert_eq!(
        directory.creature_id_by_username("robert@conformance"),
        Ok(None)
    );
    assert_eq!(
        ids(directory.creatures(None, 0, None).unwrap()),
        std::slice::from_ref(&alice.id)
    );
    directory.delete(&bob.id).unwrap();

    // A deleted identity can be registered again (legacy allows it; compensations
    // rely on it).
    let revived = CreatureRecord {
        public_key: keys[2].to_owned(),
        username: "bob-again@conformance".to_owned(),
        ..bob.clone()
    };
    directory.create(&revived).unwrap();
    assert_eq!(directory.creature(&bob.id), Ok(Some(revived)));
    directory.delete(&bob.id).unwrap();

    // A deleted creature's username can be registered again.
    let successor = CreatureRecord {
        id: "4@conformance".to_owned(),
        username: "robert@conformance".to_owned(),
        public_key: keys[1].to_owned(),
        ..bob
    };
    directory.create(&successor).unwrap();
    balances.open(&successor.id, 0).unwrap();
    assert_eq!(
        directory.creature_id_by_username("robert@conformance"),
        Ok(Some(successor.id))
    );
}

/// Exercises [`CreatureMetadata`] for a creature the suite registers itself.
///
/// Documents are compared as compact JSON with members in sorted order, the text both
/// adapters produce. `key` is an RSA SPKI public key in standard LF PEM spelling that
/// no other creature uses (the target schema keeps public keys unique).
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn creature_metadata(
    metadata: &dyn crate::CreatureMetadata,
    directory: &dyn CreatureDirectory,
    key: &str,
) {
    use aseman_domain::creature::MetadataKind::{Creature, User};
    let owner = CreatureRecord {
        id: "5@metadata".to_owned(),
        creature_type: "human".to_owned(),
        username: "meta@conformance".to_owned(),
        public_key: key.to_owned(),
        chain_id: "main".to_owned(),
        subchain_id: "main".to_owned(),
        owner_id: "free".to_owned(),
    };
    directory.create(&owner).unwrap();
    let id = owner.id.as_str();
    let read = |kind, path: &str| metadata.metadata(kind, id, path).unwrap();
    assert_eq!(read(Creature, "metadata"), None);

    let document = r#"{"public":{"profile":{"name":"a"}},"ratio":2.5,"tags":[1,null]}"#;
    metadata.replace_metadata(Creature, id, document).unwrap();
    assert_eq!(read(Creature, "metadata").as_deref(), Some(document));
    assert_eq!(
        read(Creature, "metadata.public.profile").as_deref(),
        Some(r#"{"name":"a"}"#)
    );
    // Only objects answer a path, as legacy `get_json` does.
    assert_eq!(read(Creature, "metadata.tags"), None);
    assert_eq!(read(Creature, "metadata.none"), None);
    // The user document is a separate document of the same creature.
    assert_eq!(read(User, "metadata"), None);
    metadata.replace_metadata(User, id, r#"{"x":1}"#).unwrap();
    assert_eq!(read(User, "metadata").as_deref(), Some(r#"{"x":1}"#));
    assert_eq!(read(Creature, "metadata").as_deref(), Some(document));

    // Replacing leaves nothing of the previous document behind.
    metadata
        .replace_metadata(Creature, id, r#"{"only":true}"#)
        .unwrap();
    assert_eq!(
        read(Creature, "metadata").as_deref(),
        Some(r#"{"only":true}"#)
    );
    assert_eq!(read(Creature, "metadata.public"), None);
    assert!(matches!(
        metadata.replace_metadata(Creature, id, "[1]"),
        Err(PortError::Failed(_))
    ));

    metadata.delete_metadata(Creature, id).unwrap();
    metadata.delete_metadata(Creature, id).unwrap();
    assert_eq!(read(Creature, "metadata"), None);
    metadata
        .replace_metadata(Creature, id, r#"{"again":1}"#)
        .unwrap();
    assert_eq!(
        read(Creature, "metadata").as_deref(),
        Some(r#"{"again":1}"#)
    );
    metadata.delete_metadata(Creature, id).unwrap();
    assert_eq!(read(User, "metadata").as_deref(), Some(r#"{"x":1}"#));
}

/// Exercises [`crate::CreatureTypes`] on an empty registry. Specs are compared as
/// compact JSON with members in sorted order.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn creature_types(types: &dyn crate::CreatureTypes) {
    assert_eq!(types.creature_type("human"), Ok(None));
    assert_eq!(types.creature_types(), Ok(Vec::new()));
    let human = r#"{"customFields":[],"desc":"A human.","initialBalance":0}"#;
    let machine = r#"{"customFields":[],"initialBalance":7}"#;
    types.put_creature_type("machine", machine).unwrap();
    types.put_creature_type("human", human).unwrap();
    assert_eq!(
        types.creature_type("human").unwrap().as_deref(),
        Some(human)
    );
    assert_eq!(
        types.creature_types(),
        Ok(vec![
            ("human".to_owned(), human.to_owned()),
            ("machine".to_owned(), machine.to_owned()),
        ])
    );
    // Replacing leaves nothing of the previous spec behind.
    types
        .put_creature_type("human", r#"{"initialBalance":5}"#)
        .unwrap();
    assert_eq!(
        types.creature_type("human").unwrap().as_deref(),
        Some(r#"{"initialBalance":5}"#)
    );
    assert!(matches!(
        types.put_creature_type("human", "5"),
        Err(PortError::Failed(_))
    ));
    // An empty spec reads as unregistered.
    types.put_creature_type("ghost", "{}").unwrap();
    assert_eq!(types.creature_type("ghost"), Ok(None));
    assert_eq!(
        types
            .creature_types()
            .unwrap()
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        ["human", "machine"]
    );
}

/// Exercises [`crate::ProgramDirectory`] on an empty directory. `machines` are two
/// existing machine creatures, since the target relates each program to its machine.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn program_directory(programs: &dyn crate::ProgramDirectory, machines: [&str; 2]) {
    use aseman_domain::program::ProgramRecord;
    let program = |id: &str, machine: &str| ProgramRecord {
        id: id.to_owned(),
        machine_id: machine.to_owned(),
        runtime: "wasm".to_owned(),
        path: format!("/{id}"),
        comment: String::new(),
    };
    let ids = |records: Vec<ProgramRecord>| {
        records
            .into_iter()
            .map(|record| record.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(programs.program("10@conformance"), Ok(None));
    // A machine may own several programs.
    let first = program("10@conformance", machines[0]);
    let second = program("11@conformance", machines[0]);
    let other = program("12@conformance", machines[1]);
    for record in [&first, &second, &other] {
        programs.create_program(record).unwrap();
    }
    assert_eq!(programs.create_program(&first), Err(PortError::Conflict));
    assert_eq!(programs.program(&first.id), Ok(Some(first.clone())));
    assert_eq!(
        ids(programs.programs_of_machine(machines[0]).unwrap()),
        ["10@conformance", "11@conformance"]
    );
    assert_eq!(
        ids(programs.programs(0, None).unwrap()),
        ["10@conformance", "11@conformance", "12@conformance"]
    );
    assert_eq!(
        ids(programs.programs(1, Some(1)).unwrap()),
        ["11@conformance"]
    );

    // Moving a program moves its relation.
    let moved = ProgramRecord {
        machine_id: machines[1].to_owned(),
        comment: "moved".to_owned(),
        ..second.clone()
    };
    programs.update_program(&moved).unwrap();
    assert_eq!(programs.program(&moved.id), Ok(Some(moved.clone())));
    assert_eq!(
        ids(programs.programs_of_machine(machines[0]).unwrap()),
        ["10@conformance"]
    );
    assert_eq!(
        ids(programs.programs_of_machine(machines[1]).unwrap()),
        ["11@conformance", "12@conformance"]
    );
    assert_eq!(
        programs.update_program(&program("19@conformance", machines[0])),
        Err(PortError::NotFound)
    );

    programs.delete_program(&first.id).unwrap();
    programs.delete_program(&first.id).unwrap();
    assert_eq!(programs.program(&first.id), Ok(None));
    // A deleted program can be registered again.
    programs.create_program(&first).unwrap();
    assert_eq!(programs.program(&first.id), Ok(Some(first.clone())));
    programs.delete_program(&first.id).unwrap();
    assert!(
        programs
            .programs_of_machine(machines[0])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ids(programs.programs(0, None).unwrap()),
        ["11@conformance", "12@conformance"]
    );
}

/// Exercises [`crate::ProgramMetadata`] for an existing program `program_id` without
/// metadata. Documents are compared as compact JSON with members in sorted order.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn program_metadata(metadata: &dyn crate::ProgramMetadata, program_id: &str) {
    let read = |path: &str| metadata.program_metadata(program_id, path).unwrap();
    assert_eq!(read("metadata"), None);
    metadata.merge_program_metadata(program_id, "{}").unwrap();
    assert_eq!(read("metadata").as_deref(), Some("{}"));
    metadata
        .merge_program_metadata(program_id, r#"{"mcp":{"tools":["a"],"v":1},"name":"p"}"#)
        .unwrap();
    metadata
        .merge_program_metadata(program_id, r#"{"mcp":{"v":2},"extra":null}"#)
        .unwrap();
    assert_eq!(
        read("metadata").as_deref(),
        Some(r#"{"extra":null,"mcp":{"tools":["a"],"v":2},"name":"p"}"#)
    );
    assert_eq!(
        read("metadata.mcp").as_deref(),
        Some(r#"{"tools":["a"],"v":2}"#)
    );
    assert_eq!(read("metadata.name"), None);
    assert!(matches!(
        metadata.merge_program_metadata(program_id, "[]"),
        Err(PortError::Failed(_))
    ));
    metadata.delete_program_metadata(program_id).unwrap();
    metadata.delete_program_metadata(program_id).unwrap();
    assert_eq!(read("metadata"), None);
    // A deleted document starts empty when written again.
    metadata
        .merge_program_metadata(program_id, r#"{"b":1}"#)
        .unwrap();
    assert_eq!(read("metadata").as_deref(), Some(r#"{"b":1}"#));
    metadata.delete_program_metadata(program_id).unwrap();
}

/// Exercises [`crate::ProgramAlarms`] for an existing program and store.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn program_alarms(alarms: &dyn crate::ProgramAlarms, program_id: &str, store_id: &str) {
    use aseman_domain::program::ProgramAlarm;
    assert_eq!(alarms.alarm(program_id), Ok(None));
    let first = ProgramAlarm {
        store_id: store_id.to_owned(),
        fire_at_millis: 1_700_000_000_000,
        data: r#"{"wake":1}"#.to_owned(),
        entity: "main".to_owned(),
    };
    alarms.set_alarm(program_id, &first).unwrap();
    assert_eq!(alarms.alarm(program_id), Ok(Some(first.clone())));
    let second = ProgramAlarm {
        fire_at_millis: 1_700_000_005_000,
        data: String::new(),
        entity: "worker".to_owned(),
        ..first.clone()
    };
    alarms.set_alarm(program_id, &second).unwrap();
    assert_eq!(alarms.alarm(program_id), Ok(Some(second)));
    alarms.clear_alarm(program_id).unwrap();
    alarms.clear_alarm(program_id).unwrap();
    assert_eq!(alarms.alarm(program_id), Ok(None));
    alarms.set_alarm(program_id, &first).unwrap();
    assert_eq!(alarms.alarm(program_id), Ok(Some(first)));
    alarms.clear_alarm(program_id).unwrap();
}

/// Exercises the store record operations of [`crate::StoreDirectory`] and
/// [`crate::StoreMetadata`] on a directory without stores. `creator` is an existing
/// creature.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn store_directory(
    stores: &dyn crate::StoreDirectory,
    metadata: &dyn crate::StoreMetadata,
    creator: &str,
) {
    use aseman_domain::store::StoreRecord;
    let store = |id: &str| StoreRecord {
        id: id.to_owned(),
        persistent_history: true,
        signal_count: 0,
        tag: "events".to_owned(),
        parent_id: String::new(),
        is_public: false,
        member_count: 1,
    };
    let ids = |records: Vec<StoreRecord>| {
        records
            .into_iter()
            .map(|record| record.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(stores.store("s-1@conformance"), Ok(None));
    let parent = store("s-1@conformance");
    let child = StoreRecord {
        parent_id: parent.id.clone(),
        is_public: true,
        ..store("s-2@conformance")
    };
    stores.create_store(&parent, creator).unwrap();
    stores.create_store(&child, creator).unwrap();
    assert_eq!(
        stores.create_store(&parent, creator),
        Err(PortError::Conflict)
    );
    assert_eq!(stores.store(&child.id), Ok(Some(child.clone())));
    assert_eq!(
        ids(stores.stores(0, None).unwrap()),
        ["s-1@conformance", "s-2@conformance"]
    );
    assert_eq!(ids(stores.stores(1, Some(5)).unwrap()), ["s-2@conformance"]);

    let renamed = StoreRecord {
        tag: "renamed".to_owned(),
        persistent_history: false,
        ..child.clone()
    };
    stores.update_store(&renamed).unwrap();
    assert_eq!(stores.store(&child.id), Ok(Some(renamed)));
    assert_eq!(
        stores.update_store(&store("s-9@conformance")),
        Err(PortError::NotFound)
    );
    stores.record_signal(&parent.id).unwrap();
    assert_eq!(stores.store(&parent.id).unwrap().unwrap().signal_count, 1);

    assert_eq!(metadata.store_metadata(&parent.id, "metadata"), Ok(None));
    metadata
        .merge_store_metadata(&parent.id, r#"{"budgetMinor":100,"public":{"title":"a"}}"#)
        .unwrap();
    metadata
        .merge_store_metadata(&parent.id, r#"{"public":{"icon":"x"}}"#)
        .unwrap();
    assert_eq!(
        metadata
            .store_metadata(&parent.id, "metadata")
            .unwrap()
            .as_deref(),
        Some(r#"{"budgetMinor":100,"public":{"icon":"x","title":"a"}}"#)
    );
    assert_eq!(
        metadata
            .store_metadata(&parent.id, "metadata.public")
            .unwrap()
            .as_deref(),
        Some(r#"{"icon":"x","title":"a"}"#)
    );
    metadata.delete_store_metadata(&parent.id).unwrap();
    assert_eq!(metadata.store_metadata(&parent.id, "metadata"), Ok(None));

    stores.delete_store(&child.id).unwrap();
    stores.delete_store(&child.id).unwrap();
    assert_eq!(stores.store(&child.id), Ok(None));
    // A deleted store can be registered again.
    stores.create_store(&child, creator).unwrap();
    assert_eq!(stores.store(&child.id), Ok(Some(child.clone())));
    stores.delete_store(&child.id).unwrap();
    assert_eq!(ids(stores.stores(0, None).unwrap()), ["s-1@conformance"]);
}

/// Exercises [`crate::GatewayRoutes`] for an existing creature `creator` (username
/// `{local_part}@…`) that owns the existing program `program_id`.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn gateway_routes(
    routes: &dyn crate::GatewayRoutes,
    creator: &str,
    local_part: &str,
    program_id: &str,
) {
    use aseman_domain::gateway::GatewayRoute;
    assert_eq!(routes.route(creator, "api"), Ok(None));
    let route = GatewayRoute {
        creature_id: creator.to_owned(),
        path: "api/v1".to_owned(),
        program_id: program_id.to_owned(),
        entity_id: "main".to_owned(),
        runtime: "wasm".to_owned(),
        pinned_vm_id: String::new(),
    };
    routes.put_route(&route).unwrap();
    assert_eq!(routes.route(creator, "api/v1"), Ok(Some(route.clone())));
    assert_eq!(
        routes.route_of_entity(program_id, "main"),
        Ok(Some((creator.to_owned(), "api/v1".to_owned())))
    );
    assert_eq!(routes.route_of_entity(program_id, "other"), Ok(None));

    // Moving the entity to another path leaves the old path to be deleted.
    let moved = GatewayRoute {
        path: "api/v2".to_owned(),
        runtime: "wasm-v2".to_owned(),
        ..route.clone()
    };
    routes.put_route(&moved).unwrap();
    assert_eq!(
        routes.route_of_entity(program_id, "main"),
        Ok(Some((creator.to_owned(), "api/v2".to_owned())))
    );
    routes.delete_route(creator, "api/v1").unwrap();
    assert_eq!(routes.route(creator, "api/v1"), Ok(None));
    // Deleting the old path keeps the reverse index that names the new one.
    assert_eq!(
        routes.route_of_entity(program_id, "main"),
        Ok(Some((creator.to_owned(), "api/v2".to_owned())))
    );
    routes.delete_route(creator, "api/v2").unwrap();
    routes.delete_route(creator, "api/v2").unwrap();
    assert_eq!(routes.route_of_entity(program_id, "main"), Ok(None));
    routes.put_route(&moved).unwrap();
    assert_eq!(routes.route(creator, "api/v2"), Ok(Some(moved.clone())));
    routes.delete_route(creator, "api/v2").unwrap();

    routes.put_alias(local_part, creator).unwrap();
    assert_eq!(routes.alias(local_part), Ok(Some(creator.to_owned())));
    assert_eq!(routes.alias("nobody-here"), Ok(None));
}

/// Exercises [`crate::VmResourceStores`] for two existing machine creatures.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn vm_resource_stores(stores: &dyn crate::VmResourceStores, machines: [&str; 2]) {
    assert_eq!(stores.resource_store("rs-1"), Ok(None));
    assert!(matches!(
        stores.put_resource_store("rs-0", "orphan", "", "{}"),
        Err(PortError::Failed(_))
    ));
    stores
        .put_resource_store("rs-1", "one", machines[0], r#"{"a":{"x":1}}"#)
        .unwrap();
    stores
        .put_resource_store("rs-2", "two", machines[1], "{}")
        .unwrap();
    // An update without a machine keeps the owner and merges the metadata.
    stores
        .put_resource_store("rs-1", "renamed", "", r#"{"a":{"y":2}}"#)
        .unwrap();
    let one = stores.resource_store("rs-1").unwrap().unwrap();
    assert_eq!(
        (
            one.name.as_str(),
            one.machine_id.as_str(),
            one.metadata.as_str()
        ),
        ("renamed", machines[0], r#"{"a":{"x":1,"y":2}}"#)
    );
    assert_eq!(
        stores.resource_stores(Some(machines[0])),
        Ok(vec!["rs-1".to_owned()])
    );
    assert_eq!(
        stores.resource_stores(None),
        Ok(vec!["rs-1".to_owned(), "rs-2".to_owned()])
    );
    stores.delete_resource_store("rs-1").unwrap();
    stores.delete_resource_store("rs-1").unwrap();
    assert_eq!(stores.resource_store("rs-1"), Ok(None));
    // A deleted store starts over when put again.
    stores
        .put_resource_store("rs-1", "again", machines[0], "{}")
        .unwrap();
    assert_eq!(
        stores.resource_store("rs-1").unwrap().unwrap().metadata,
        "{}"
    );
    stores.delete_resource_store("rs-1").unwrap();
    assert!(
        stores
            .resource_stores(Some(machines[0]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(stores.resource_stores(None), Ok(vec!["rs-2".to_owned()]));
}

/// Exercises [`crate::StoreAccess`] for an existing store and two existing creatures
/// that are not yet members.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn store_access(access: &dyn crate::StoreAccess, store_id: &str, members: [&str; 2]) {
    use aseman_domain::store_permissions::StorePermissions;
    let [first, second] = members;
    assert_eq!(access.is_member(store_id, first), Ok(false));
    assert_eq!(
        access.permissions(store_id, first),
        Ok(StorePermissions::default())
    );
    access
        .join(store_id, first, StorePermissions::member())
        .unwrap();
    access
        .join(store_id, second, StorePermissions::viewer())
        .unwrap();
    assert_eq!(access.is_member(store_id, first), Ok(true));
    assert_eq!(
        access.members(store_id),
        Ok(vec![
            (first.to_owned(), StorePermissions::member()),
            (second.to_owned(), StorePermissions::viewer()),
        ])
    );
    assert!(
        access
            .stores_of(first)
            .unwrap()
            .contains(&store_id.to_owned())
    );
    access
        .set_permissions(store_id, second, StorePermissions::owner())
        .unwrap();
    assert_eq!(
        access.permissions(store_id, second),
        Ok(StorePermissions::owner())
    );
    access.leave(store_id, first).unwrap();
    access.leave(store_id, first).unwrap();
    assert_eq!(access.is_member(store_id, first), Ok(false));
    assert!(
        !access
            .stores_of(first)
            .unwrap()
            .contains(&store_id.to_owned())
    );
    // Re-joining after leaving is a fresh membership.
    access
        .join(store_id, first, StorePermissions::viewer())
        .unwrap();
    assert_eq!(
        access.permissions(store_id, first),
        Ok(StorePermissions::viewer())
    );
    access.leave(store_id, first).unwrap();
    access.leave(store_id, second).unwrap();
    assert!(access.members(store_id).unwrap().is_empty());
}

/// Exercises [`crate::BlobStore`] on an empty store.
///
/// # Panics
///
/// Panics when the provider deviates from the port contract.
pub fn blob_store(blobs: &dyn crate::BlobStore) {
    let key = "machines/10@c/entities/main/module.wasm";
    assert_eq!(blobs.blob(key), Ok(None));
    assert_eq!(blobs.has_blob(key), Ok(false));
    let first = blobs
        .put_blob(key, b"\0asm-one", "application/wasm", false)
        .unwrap();
    assert_eq!(
        (
            first.store_key.as_str(),
            first.size_bytes,
            first.media_type.as_str()
        ),
        (key, 8, "application/wasm")
    );
    assert_eq!(blobs.blob(key), Ok(Some(b"\0asm-one".to_vec())));
    assert_eq!(blobs.has_blob(key), Ok(true));
    assert!(blobs.local_path(key).unwrap().ends_with("module.wasm"));
    assert_eq!(
        blobs.put_blob(key, b"other", "application/wasm", false),
        Err(PortError::Conflict)
    );
    let second = blobs
        .put_blob(key, b"\0asm-two", "application/wasm", true)
        .unwrap();
    assert_ne!(first.content_digest, second.content_digest);
    // The same bytes always give the same evidence.
    assert_eq!(
        blobs
            .put_blob(key, b"\0asm-two", "application/wasm", true)
            .unwrap(),
        second
    );
    for invalid in ["../escape", "/abs", "a//b"] {
        assert!(matches!(
            blobs.put_blob(invalid, b"x", "text/plain", true),
            Err(PortError::Failed(_))
        ));
    }
    blobs.delete_blob(key).unwrap();
    blobs.delete_blob(key).unwrap();
    assert_eq!(blobs.blob(key), Ok(None));
}

/// Exercises [`crate::EntityDirectory`] for an existing program without entities.
/// Artifact keys lie under the program's entity folder, so a provider that records
/// files by local path can map them back.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn entity_directory(entities: &dyn crate::EntityDirectory, program_id: &str) {
    use aseman_domain::blob::BlobEvidence;
    use aseman_domain::program::{ArtifactRole, EntityArtifact, EntityRecord};
    let entity = |entity_id: &str, entity_type: &str| EntityRecord {
        program_id: program_id.to_owned(),
        entity_id: entity_id.to_owned(),
        entity_type: entity_type.to_owned(),
        image_name: entity_id.to_owned(),
    };
    let evidence = |entity_id: &str, name: &str, digest: u8| BlobEvidence {
        store_key: format!("machines/{program_id}/entities/{entity_id}/{name}"),
        content_digest: [digest; 32],
        size_bytes: 4,
        media_type: "application/octet-stream".to_owned(),
    };
    let stored = |evidence: &BlobEvidence| {
        Some(EntityArtifact {
            store_key: Some(evidence.store_key.clone()),
        })
    };
    let listed = |entities: &dyn crate::EntityDirectory| {
        entities
            .deployed_programs()
            .unwrap()
            .into_iter()
            .filter(|program| program == program_id)
            .count()
    };

    assert_eq!(entities.entity(program_id, "main"), Ok(None));
    assert_eq!(
        entities.artifact(program_id, "main", ArtifactRole::Primary),
        Ok(None)
    );
    assert_eq!(entities.entity_config(program_id, "main"), Ok(None));
    assert_eq!(listed(entities), 0);
    let orphan = EntityRecord {
        program_id: "missing@conformance".to_owned(),
        ..entity("main", "wasm")
    };
    assert_eq!(entities.put_entity(&orphan), Err(PortError::NotFound));
    assert_eq!(
        entities.put_artifact(
            program_id,
            "main",
            ArtifactRole::Primary,
            &evidence("main", "module.wasm", 1)
        ),
        Err(PortError::NotFound)
    );
    assert_eq!(
        entities.merge_entity_config(program_id, "main", "{}"),
        Err(PortError::NotFound)
    );

    entities.put_entity(&entity("main", "wasm")).unwrap();
    assert_eq!(
        entities.entity(program_id, "main"),
        Ok(Some(entity("main", "wasm")))
    );
    // An entity without a primary file is not deployed.
    assert_eq!(listed(entities), 0);
    let first = evidence("main", "module.wasm", 1);
    entities
        .put_artifact(program_id, "main", ArtifactRole::Primary, &first)
        .unwrap();
    assert_eq!(
        entities.artifact(program_id, "main", ArtifactRole::Primary),
        Ok(stored(&first))
    );
    assert_eq!(
        entities.artifact(program_id, "main", ArtifactRole::Downloadable),
        Ok(None)
    );
    assert_eq!(listed(entities), 1);

    // A redeploy replaces the entity and its file.
    let redeployed = EntityRecord {
        image_name: "image".to_owned(),
        ..entity("main", "javascript")
    };
    entities.put_entity(&redeployed).unwrap();
    let second = evidence("main", "index.js", 2);
    entities
        .put_artifact(program_id, "main", ArtifactRole::Primary, &second)
        .unwrap();
    entities
        .put_artifact(program_id, "main", ArtifactRole::Downloadable, &second)
        .unwrap();
    assert_eq!(entities.entity(program_id, "main"), Ok(Some(redeployed)));
    assert_eq!(
        entities.artifact(program_id, "main", ArtifactRole::Primary),
        Ok(stored(&second))
    );
    assert_eq!(
        entities.artifact(program_id, "main", ArtifactRole::Downloadable),
        Ok(stored(&second))
    );

    // Each program is listed once, whatever its number of deployed entities.
    entities.put_entity(&entity("worker", "wasm")).unwrap();
    entities
        .put_artifact(
            program_id,
            "worker",
            ArtifactRole::Primary,
            &evidence("worker", "module.wasm", 3),
        )
        .unwrap();
    assert_eq!(listed(entities), 1);
    let programs = entities.deployed_programs().unwrap();
    let mut sorted = programs.clone();
    sorted.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    sorted.dedup();
    assert_eq!(programs, sorted);

    entities
        .merge_entity_config(
            program_id,
            "main",
            r#"{"targetProgramId":"t","inject":{"a":1}}"#,
        )
        .unwrap();
    entities
        .merge_entity_config(program_id, "main", r#"{"inject":{"b":2}}"#)
        .unwrap();
    assert_eq!(
        entities
            .entity_config(program_id, "main")
            .unwrap()
            .as_deref(),
        Some(r#"{"inject":{"a":1,"b":2},"targetProgramId":"t"}"#)
    );
    assert_eq!(entities.entity_config(program_id, "worker"), Ok(None));
    assert!(matches!(
        entities.merge_entity_config(program_id, "main", "[]"),
        Err(PortError::Failed(_))
    ));
}

/// Exercises [`crate::VmResourceEntities`] for an existing resource store without
/// entities.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn vm_resource_entities(entities: &dyn crate::VmResourceEntities, store_id: &str) {
    use aseman_domain::blob::BlobEvidence;
    use aseman_domain::program::{ResourceEntityRef, VmResourceEntity};
    let reference = |store: &str, entity_id: &str| ResourceEntityRef {
        store_id: store.to_owned(),
        entity_type: "doc".to_owned(),
        entity_id: entity_id.to_owned(),
    };
    let data = |reference: &ResourceEntityRef, digest: u8| BlobEvidence {
        store_key: reference.data_key(),
        content_digest: [digest; 32],
        size_bytes: 2,
        media_type: "application/json".to_owned(),
    };
    let first = reference(store_id, "e-1");

    assert_eq!(entities.resource_entity(&first), Ok(None));
    let orphan = reference("missing-store", "e-1");
    assert_eq!(
        entities.put_resource_entity(&orphan, "{}", &data(&orphan, 1)),
        Err(PortError::NotFound)
    );
    let invalid = ResourceEntityRef {
        entity_type: "../..".to_owned(),
        ..first.clone()
    };
    assert!(matches!(
        entities.put_resource_entity(&invalid, "{}", &data(&first, 1)),
        Err(PortError::Failed(_))
    ));
    assert!(matches!(
        entities.put_resource_entity(&first, "[]", &data(&first, 1)),
        Err(PortError::Failed(_))
    ));

    entities
        .put_resource_entity(&first, r#"{"a":{"x":1},"b":true}"#, &data(&first, 1))
        .unwrap();
    entities
        .put_resource_entity(&first, r#"{"a":{"y":2}}"#, &data(&first, 2))
        .unwrap();
    assert_eq!(
        entities.resource_entity(&first),
        Ok(Some(VmResourceEntity {
            reference: first.clone(),
            payload: r#"{"a":{"x":1,"y":2},"b":true}"#.to_owned(),
            data_key: Some(first.data_key()),
        }))
    );
    let second = reference(store_id, "e-2");
    entities
        .put_resource_entity(&second, "{}", &data(&second, 3))
        .unwrap();
    entities.delete_resource_entity(&first).unwrap();
    entities.delete_resource_entity(&first).unwrap();
    assert_eq!(entities.resource_entity(&first), Ok(None));
    assert!(entities.resource_entity(&second).unwrap().is_some());
    // A deleted entity starts empty when written again.
    entities
        .put_resource_entity(&first, r#"{"c":3}"#, &data(&first, 4))
        .unwrap();
    assert_eq!(
        entities
            .resource_entity(&first)
            .unwrap()
            .map(|entity| entity.payload),
        Some(r#"{"c":3}"#.to_owned())
    );
    entities.delete_resource_entity(&first).unwrap();
    entities.delete_resource_entity(&second).unwrap();
}

/// Exercises [`crate::KeyDirectory`] on an empty directory.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn key_directory(keys: &dyn crate::KeyDirectory) {
    use aseman_domain::identity::{IdentityKey, KeyEpoch, KeyPurpose, Subject, SubjectKind};
    let subject = Subject {
        kind: SubjectKind::Workload,
        id: "0190f1a2-7b3c-7d4e-8f00-00000000c0f1".parse().unwrap(),
    };
    let key = |key_id: &str, epoch: u32, subject: Subject| IdentityKey {
        key_id: key_id.to_owned(),
        public_key: vec![1, 0xed, 0x01, epoch as u8],
        epoch: KeyEpoch {
            subject,
            purpose: KeyPurpose::Authentication,
            epoch,
            not_before_millis: 1_000,
            expires_at_millis: Some(9_000_000),
            retired_at_millis: None,
            revoked_at_millis: None,
            legacy: false,
        },
    };
    let first = key("zQmKeyOne", 1, subject);
    let second = key("zQmKeyTwo", 2, subject);

    assert_eq!(keys.key("zQmKeyOne"), Ok(None));
    assert_eq!(
        keys.epochs(&subject, KeyPurpose::Authentication),
        Ok(Vec::new())
    );
    keys.register(&second).unwrap();
    keys.register(&first).unwrap();
    assert_eq!(keys.key("zQmKeyOne"), Ok(Some(first.clone())));
    assert_eq!(
        keys.epochs(&subject, KeyPurpose::Authentication),
        Ok(vec![first.clone(), second.clone()])
    );
    assert_eq!(
        keys.epochs(&subject, KeyPurpose::Descriptor),
        Ok(Vec::new())
    );
    // A key ID or an epoch is recorded once.
    assert_eq!(keys.register(&first), Err(PortError::Conflict));
    assert_eq!(
        keys.register(&key("zQmOtherId", 1, subject)),
        Err(PortError::Conflict)
    );
    let other_subject = Subject {
        kind: SubjectKind::Creature,
        ..subject
    };
    assert_eq!(
        keys.register(&key("zQmKeyOne", 1, other_subject)),
        Err(PortError::Conflict)
    );
    keys.register(&key("zQmCreature", 1, other_subject))
        .unwrap();
    assert_eq!(
        keys.epochs(&subject, KeyPurpose::Authentication)
            .unwrap()
            .len(),
        2
    );

    // Retirement and revocation keep the earliest time.
    keys.retire("zQmKeyOne", 5_000).unwrap();
    keys.retire("zQmKeyOne", 7_000).unwrap();
    keys.revoke("zQmKeyOne", 6_000).unwrap();
    keys.revoke("zQmKeyOne", 4_000).unwrap();
    let stored = keys.key("zQmKeyOne").unwrap().unwrap();
    assert_eq!(stored.epoch.retired_at_millis, Some(5_000));
    assert_eq!(stored.epoch.revoked_at_millis, Some(4_000));
    assert_eq!(keys.retire("zQmMissing", 1), Err(PortError::NotFound));
    assert_eq!(keys.revoke("zQmMissing", 1), Err(PortError::NotFound));
}

/// Exercises [`crate::ReplayGuard`] on an empty guard.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn replay_guard(guard: &dyn crate::ReplayGuard) {
    let nonce = [7u8; 16];
    assert_eq!(guard.record_nonce("zQmKey", &nonce, 2_000, 1_000), Ok(true));
    assert_eq!(
        guard.record_nonce("zQmKey", &nonce, 2_000, 1_500),
        Ok(false)
    );
    // The same nonce under another key is another pair.
    assert_eq!(
        guard.record_nonce("zQmOther", &nonce, 2_000, 1_500),
        Ok(true)
    );
    assert_eq!(
        guard.record_nonce("zQmKey", &[8u8; 16], 2_000, 1_500),
        Ok(true)
    );
    // After its retention the pair may be recorded again.
    assert_eq!(guard.record_nonce("zQmKey", &nonce, 4_000, 2_000), Ok(true));
    assert_eq!(
        guard.record_nonce("zQmKey", &nonce, 4_000, 3_999),
        Ok(false)
    );
}

/// Exercises [`crate::ChallengeStore`] on an empty store.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn challenge_store(challenges: &dyn crate::ChallengeStore) {
    use aseman_domain::identity::{Subject, SubjectKind};
    let subject = Subject {
        kind: SubjectKind::Creature,
        id: "0190f1a2-7b3c-7d4e-8f00-0000000c4a11".parse().unwrap(),
    };
    let other = Subject {
        kind: SubjectKind::User,
        ..subject
    };
    let first = challenges.issue(&subject, "node:a", 2_000).unwrap();
    let second = challenges.issue(&subject, "node:a", 2_000).unwrap();
    assert_eq!(first.nonce.len(), 32);
    assert_ne!(first.nonce, second.nonce);
    assert_eq!(
        (
            first.subject,
            first.audience.as_str(),
            first.expires_at_millis
        ),
        (subject, "node:a", 2_000)
    );
    // Another subject or audience cannot consume it, and does not use it up.
    assert_eq!(
        challenges.consume(&first.nonce, &other, "node:a", 1_000),
        Ok(false)
    );
    assert_eq!(
        challenges.consume(&first.nonce, &subject, "node:b", 1_000),
        Ok(false)
    );
    assert_eq!(
        challenges.consume(&first.nonce, &subject, "node:a", 1_000),
        Ok(true)
    );
    assert_eq!(
        challenges.consume(&first.nonce, &subject, "node:a", 1_000),
        Ok(false)
    );
    // An expired challenge is never accepted.
    assert_eq!(
        challenges.consume(&second.nonce, &subject, "node:a", 2_000),
        Ok(false)
    );
    assert_eq!(
        challenges.consume(&[0; 32], &subject, "node:a", 1_000),
        Ok(false)
    );
}

/// Exercises [`crate::GrantStore`] on an empty store.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn grant_store(grants: &dyn crate::GrantStore) {
    use aseman_domain::Uuid;
    use aseman_domain::capability::{Grant, ResourceSelector};
    use aseman_domain::identity::{Subject, SubjectKind};
    let holder = Subject {
        kind: SubjectKind::User,
        id: "0190f1a2-7b3c-7d4e-8f00-0000000067a1".parse().unwrap(),
    };
    let workload = Subject {
        kind: SubjectKind::Workload,
        id: "0190f1a2-7b3c-7d4e-8f00-0000000067a2".parse().unwrap(),
    };
    let root = Grant {
        id: Uuid::from_u128(0x67a0_0001),
        subject: holder,
        issuer: holder,
        actions: ["store.signal".to_owned(), "store.read".to_owned()].into(),
        resource: ResourceSelector::AnyOfKind {
            kind: "store".to_owned(),
        },
        delegable_actions: ["store.signal".to_owned()].into(),
        max_depth: 2,
        parent: None,
        not_before_millis: 1_000,
        expires_at_millis: Some(9_000_000),
        revoked_at_millis: None,
        policy_version: "p1".to_owned(),
    };
    let child = Grant {
        id: Uuid::from_u128(0x67a0_0002),
        subject: workload,
        issuer: holder,
        actions: ["store.signal".to_owned()].into(),
        resource: ResourceSelector::Exact {
            kind: "store".to_owned(),
            id: "s1".to_owned(),
        },
        delegable_actions: Default::default(),
        max_depth: 0,
        parent: Some(root.id),
        not_before_millis: 2_000,
        expires_at_millis: None,
        revoked_at_millis: None,
        policy_version: "p1".to_owned(),
    };
    assert_eq!(grants.grant(root.id), Ok(None));
    assert_eq!(grants.grants_of(&holder), Ok(Vec::new()));
    grants.put(&root).unwrap();
    grants.put(&child).unwrap();
    assert_eq!(grants.put(&root), Err(PortError::Conflict));
    assert_eq!(grants.grant(root.id), Ok(Some(root.clone())));
    assert_eq!(grants.grant(child.id), Ok(Some(child.clone())));
    assert_eq!(grants.grants_of(&holder), Ok(vec![root.clone()]));
    assert_eq!(grants.grants_of(&workload), Ok(vec![child.clone()]));
    assert_eq!(grants.children(root.id), Ok(vec![child.clone()]));
    assert_eq!(grants.children(child.id), Ok(Vec::new()));
    grants.revoke(root.id, 5_000).unwrap();
    grants.revoke(root.id, 7_000).unwrap();
    assert_eq!(
        grants.grant(root.id).unwrap().unwrap().revoked_at_millis,
        Some(5_000)
    );
    assert_eq!(
        grants.revoke(Uuid::from_u128(0x67a0_0099), 1),
        Err(PortError::NotFound)
    );
}

/// Exercises [`crate::CreatureDatabaseBindings`] for a creature without a binding.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn creature_database_bindings(
    bindings: &dyn crate::CreatureDatabaseBindings,
    creature: aseman_domain::CreatureId,
) {
    use aseman_domain::{BindingStatus, CreatureDatabaseBinding, Generation};
    assert_eq!(bindings.binding_for(creature), Ok(None));
    let first = CreatureDatabaseBinding::new(
        creature,
        "postgres-guest-v1".to_owned(),
        "aseman_c_one".to_owned(),
        "aseman_r_one".to_owned(),
    )
    .unwrap();
    bindings.record_binding(&first).unwrap();
    assert_eq!(bindings.binding_for(creature), Ok(Some(first.clone())));
    let active = CreatureDatabaseBinding {
        status: BindingStatus::Active,
        ..first.clone()
    };
    bindings.record_binding(&active).unwrap();
    assert_eq!(bindings.binding_for(creature), Ok(Some(active.clone())));
    let next = CreatureDatabaseBinding {
        database: "aseman_c_two".to_owned(),
        role: "aseman_r_two".to_owned(),
        generation: Generation::INITIAL.next().unwrap(),
        ..active.clone()
    };
    bindings.record_binding(&next).unwrap();
    assert_eq!(bindings.binding_for(creature), Ok(Some(next)));
    // A generation never moves backwards.
    assert_eq!(bindings.record_binding(&active), Err(PortError::Conflict));
}

/// Exercises [`crate::GuestKv`] on an active binding with no guest pairs.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn guest_kv(kv: &dyn crate::GuestKv, binding: &aseman_domain::CreatureDatabaseBinding) {
    use aseman_domain::guest::{GuestKvOperation as Op, GuestKvOutcome as Out, LegacyKvNamespace};
    let run = |operation: Op| kv.execute(binding, &operation).unwrap();
    let get = |namespace, key: &str| {
        run(Op::Get {
            namespace,
            key: key.to_owned(),
        })
    };
    let put = |namespace, key: &str, value: &str| {
        run(Op::Put {
            namespace,
            key: key.to_owned(),
            value: value.to_owned(),
        })
    };
    let list = |namespace, prefix: &str, limit| {
        run(Op::List {
            namespace,
            prefix: prefix.to_owned(),
            limit,
        })
    };
    let (dbop, applet) = (LegacyKvNamespace::DbOp, LegacyKvNamespace::AppletDb);
    assert_eq!(get(dbop, "profile"), Out::Value { value: None });
    assert_eq!(put(dbop, "profile", "v1"), Out::Written);
    assert_eq!(put(dbop, "profile", "v2"), Out::Written);
    assert_eq!(
        get(dbop, "profile"),
        Out::Value {
            value: Some("v2".to_owned())
        }
    );
    // The namespaces are separate key spaces.
    assert_eq!(get(applet, "profile"), Out::Value { value: None });
    put(applet, "profile", "applet");
    for key in ["user:b", "user:a", "user_c", "other"] {
        put(dbop, key, key);
    }
    // Committed pairs, in key order, limited, and prefix-exact (`_` is not a wildcard).
    assert_eq!(
        list(dbop, "user:", 10),
        Out::Listed {
            pairs: vec![
                ("user:a".to_owned(), "user:a".to_owned()),
                ("user:b".to_owned(), "user:b".to_owned()),
            ]
        }
    );
    assert_eq!(
        list(dbop, "user", 2),
        Out::Listed {
            pairs: vec![
                ("user:a".to_owned(), "user:a".to_owned()),
                ("user:b".to_owned(), "user:b".to_owned()),
            ]
        }
    );
    // Deletes really delete, and a deleted key can be written again.
    assert_eq!(
        run(Op::Delete {
            namespace: dbop,
            key: "profile".to_owned()
        }),
        Out::Deleted { existed: true }
    );
    assert_eq!(get(dbop, "profile"), Out::Value { value: None });
    assert_eq!(
        run(Op::Delete {
            namespace: dbop,
            key: "profile".to_owned()
        }),
        Out::Deleted { existed: false }
    );
    assert!(!matches!(list(dbop, "prof", 10), Out::Listed { pairs } if !pairs.is_empty()));
    put(dbop, "profile", "v3");
    assert_eq!(
        get(dbop, "profile"),
        Out::Value {
            value: Some("v3".to_owned())
        }
    );
    assert_eq!(
        get(applet, "profile"),
        Out::Value {
            value: Some("applet".to_owned())
        }
    );

    // Documents (ADR 0028): the legacy JSON store's records, one row each.
    let put_json = |key: &str, path: &str, data: &str, merge: bool| {
        kv.execute(
            binding,
            &Op::PutJson {
                key: key.to_owned(),
                path: path.to_owned(),
                data: data.to_owned(),
                merge,
            },
        )
    };
    let get_json = |key: &str, path: &str| {
        run(Op::GetJson {
            key: key.to_owned(),
            path: path.to_owned(),
        })
    };
    let keys = |prefix: &str| {
        run(Op::ListJson {
            prefix: prefix.to_owned(),
            limit: 100,
        })
    };
    let document = |text: &str| Out::Document {
        data: text.to_owned(),
    };
    let listed = |items: &[&str]| Out::Keys {
        keys: items.iter().map(|item| (*item).to_owned()).collect(),
    };
    assert_eq!(get_json("counter", "doc"), document("{}"));
    put_json("counter", "doc", r#"{"n":1}"#, true).unwrap();
    assert_eq!(get_json("counter", "doc"), document(r#"{"n":1}"#));
    assert_eq!(keys(""), listed(&["counter::doc", "counter::doc.n"]));
    put_json("counter", "doc", r#"{"m":{"x":true}}"#, true).unwrap();
    assert_eq!(
        get_json("counter", "doc"),
        document(r#"{"m":{"x":true},"n":1}"#)
    );
    assert_eq!(get_json("counter", "doc.m"), document(r#"{"x":true}"#));
    assert_eq!(
        keys("counter::doc."),
        listed(&["counter::doc.m", "counter::doc.m.x", "counter::doc.n"])
    );
    // Without merge the object at the path is replaced.
    put_json("counter", "doc", r#"{"z":0}"#, false).unwrap();
    assert_eq!(get_json("counter", "doc"), document(r#"{"z":0}"#));
    // Deleting a subtree, then the whole document.
    put_json("other", "doc", r#"{"a":1}"#, true).unwrap();
    run(Op::DeleteJson {
        key: "counter".to_owned(),
        path: "doc.m".to_owned(),
    });
    assert_eq!(get_json("counter", "doc.m"), document("{}"));
    run(Op::DeleteJson {
        key: "counter".to_owned(),
        path: String::new(),
    });
    assert_eq!(keys("counter::"), listed(&[]));
    assert_eq!(keys(""), listed(&["other::doc", "other::doc.a"]));
    // A document root must be an object.
    assert!(put_json("counter", "doc", "[1]", true).is_err());
    // Documents and the other namespaces stay apart.
    assert_eq!(
        get(dbop, "profile"),
        Out::Value {
            value: Some("v3".to_owned())
        }
    );
}

/// Exercises [`crate::DecisionAudit`] on an empty log.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn decision_audit(audit: &dyn crate::DecisionAudit) {
    use aseman_domain::authority::AuditRecord;
    let record = |actor: &str, decision: &str, at: i64| AuditRecord {
        actor: actor.to_owned(),
        action: "store.signal".to_owned(),
        target: "store:s1".to_owned(),
        decision: decision.to_owned(),
        occurred_at_millis: at,
        details: r#"{"matched":"store_signal"}"#.to_owned(),
    };
    let alice = "workload:0190f1a2-7b3c-7d4e-8f00-0000000a0d17";
    let bob = "workload:0190f1a2-7b3c-7d4e-8f00-0000000a0d18";
    assert_eq!(audit.stream(alice), Ok(Vec::new()));
    assert_eq!(audit.record(&record(alice, "allowed", 1_000)), Ok(1));
    assert_eq!(
        audit.record(&record(alice, "condition_not_met", 2_000)),
        Ok(2)
    );
    assert_eq!(audit.record(&record(bob, "allowed", 1_500)), Ok(1));
    let stream = audit.stream(alice).unwrap();
    assert_eq!(
        stream
            .iter()
            .map(|entry| (entry.sequence, entry.record.decision.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "allowed"), (2, "condition_not_met")]
    );
    assert_eq!(stream[1].record, record(alice, "condition_not_met", 2_000));
    assert_eq!(audit.stream(bob).unwrap().len(), 1);
}

/// Exercises [`crate::WorkloadRepository`]: `program` belongs to `creature`, and
/// `foreign_program` to another creature.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn workload_repository(
    workloads: &dyn crate::WorkloadRepository,
    creature: aseman_domain::CreatureId,
    program: aseman_domain::ProgramId,
    foreign_program: aseman_domain::ProgramId,
) {
    use aseman_domain::{DesiredWorkload, DesiredWorkloadState, Generation, WorkloadId};
    let workload = DesiredWorkload {
        id: WorkloadId::new(),
        creature_id: creature,
        program_id: program,
        name: "main/vm-1".to_owned(),
        runtime: "wasm".to_owned(),
        generation: Generation::INITIAL,
        state: DesiredWorkloadState::Running,
    };
    assert_eq!(workloads.get_desired(workload.id), Ok(None));
    workloads.create_desired(&workload).unwrap();
    assert_eq!(
        workloads.get_desired(workload.id),
        Ok(Some(workload.clone()))
    );
    assert_eq!(
        workloads.create_desired(&workload),
        Err(PortError::Conflict),
        "an ID exists once"
    );
    let same_name = DesiredWorkload {
        id: WorkloadId::new(),
        ..workload.clone()
    };
    assert_eq!(
        workloads.create_desired(&same_name),
        Err(PortError::Conflict),
        "a name exists once per program"
    );
    let crossed = DesiredWorkload {
        id: WorkloadId::new(),
        program_id: foreign_program,
        name: "main/vm-2".to_owned(),
        ..workload.clone()
    };
    assert!(
        workloads.create_desired(&crossed).is_err(),
        "a workload never joins another creature's program"
    );
    assert_eq!(workloads.get_desired(crossed.id), Ok(None));
    let mut stopped = workload.clone();
    stopped.state = DesiredWorkloadState::Stopped;
    stopped.generation = Generation::INITIAL.next().unwrap();
    assert_eq!(
        workloads.put_desired(&stopped, stopped.generation),
        Err(PortError::Conflict),
        "compare-and-set on the generation read"
    );
    workloads
        .put_desired(&stopped, Generation::INITIAL)
        .unwrap();
    assert_eq!(workloads.get_desired(workload.id), Ok(Some(stopped)));
}
