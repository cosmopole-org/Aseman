//! Behavioral suites every adapter of a port must pass (feature `conformance`).
//!
//! The legacy adapters and the capsule adapters run the same suite, so a use case
//! behaves identically before and after cutover.

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
