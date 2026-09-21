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
