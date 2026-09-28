//! The behavior every storage provider plugin must show (ADR 0036), run through the
//! engine on the real model catalog. `fresh` must be an empty store.

use crate::engine::Storage;
use crate::error::StorageError;
use crate::provider::Mode;
use crate::query::{Case, Cond, FindMany, Order, Unique, Where};
use crate::value::{Data, Value};

fn user(name: &str, email: Option<&str>, key: u8) -> Data {
    let mut data = Data::from([
        ("username".to_owned(), Value::from(name)),
        ("public_key".to_owned(), Value::Bytes(vec![key; 8])),
        ("status".to_owned(), Value::from("active")),
    ]);
    if let Some(email) = email {
        data.insert("email".to_owned(), Value::from(email));
    }
    data
}

fn names(rows: &[crate::value::Row]) -> Vec<String> {
    rows.iter()
        .map(|row| row.text("username").unwrap_or_default().to_owned())
        .collect()
}

/// # Panics
///
/// Panics when the provider deviates from the contract.
pub fn storage_provider(storage: &Storage) {
    crud(storage);
    queries(storage);
    transactions(storage);
    relations_and_documents(storage);
    migration(storage);
    aseman_ports::conformance::consensus_log::consensus_log(
        &*storage.provider().consensus_logs(),
        "conformance-log",
    );
}

fn crud(storage: &Storage) {
    let trx = storage.begin(Mode::ReadWrite).unwrap();
    let ada = trx.create("core.user", user("ada", Some("ada@x.io"), 1)).unwrap();
    assert_eq!(ada.revision, 1);
    assert_eq!(
        trx.find_unique("core.user", &Unique::Id(ada.id)).unwrap(),
        Some(ada.clone()),
        "a transaction reads its own writes"
    );
    assert_eq!(
        trx.find_unique("core.user", &Unique::fields([("username", "ada")]))
            .unwrap()
            .map(|row| row.id),
        Some(ada.id)
    );
    // Unique indexes hold.
    assert!(matches!(
        trx.create("core.user", user("ada", None, 2)),
        Err(StorageError::Conflict(_))
    ));
    // Update merges; null clears.
    let updated = trx
        .update(
            "core.user",
            &Unique::Id(ada.id),
            Data::from([
                ("status".to_owned(), Value::from("away")),
                ("email".to_owned(), Value::Null),
            ]),
        )
        .unwrap()
        .unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.text("status"), Some("away"));
    assert_eq!(updated.text("username"), Some("ada"));
    assert!(updated.get("email").is_null());
    // A required field cannot be cleared; an unknown field is refused.
    assert!(trx
        .update(
            "core.user",
            &Unique::Id(ada.id),
            Data::from([("status".to_owned(), Value::Null)])
        )
        .is_err());
    assert!(trx.create("core.user", Data::from([("nope".to_owned(), Value::Int(1))])).is_err());
    // Upsert creates, then updates.
    let bob = trx
        .upsert(
            "core.user",
            &Unique::fields([("username", "bob")]),
            user("bob", None, 3),
            Data::from([("status".to_owned(), Value::from("x"))]),
        )
        .unwrap();
    assert_eq!(bob.revision, 1);
    let bob = trx
        .upsert(
            "core.user",
            &Unique::fields([("username", "bob")]),
            user("bob", None, 3),
            Data::from([("status".to_owned(), Value::from("busy"))]),
        )
        .unwrap();
    assert_eq!((bob.revision, bob.text("status")), (2, Some("busy")));
    // Delete, then the name is free again.
    assert!(trx.delete("core.user", &Unique::Id(bob.id)).unwrap().is_some());
    assert_eq!(trx.find_unique("core.user", &Unique::Id(bob.id)).unwrap(), None);
    assert!(trx.delete("core.user", &Unique::Id(bob.id)).unwrap().is_none());
    trx.create("core.user", user("bob", None, 4)).unwrap();
    trx.commit().unwrap();

    let reader = storage.begin(Mode::ReadOnly).unwrap();
    assert_eq!(reader.count("core.user", None).unwrap(), 2);
    assert!(reader.create("core.user", user("eve", None, 9)).is_err());
    reader.rollback().unwrap();
    wipe(storage, "core.user");
}

fn wipe(storage: &Storage, model: &str) {
    let trx = storage.begin(Mode::ReadWrite).unwrap();
    trx.delete_many(model, None).unwrap();
    trx.commit().unwrap();
}

fn queries(storage: &Storage) {
    let trx = storage.begin(Mode::ReadWrite).unwrap();
    for (index, name) in ["carol", "Alice", "bob", "dave", "Eve"].iter().enumerate() {
        let email = (index % 2 == 0).then(|| format!("{name}@mail.io"));
        trx.create(
            "core.user",
            user(name, email.as_deref(), u8::try_from(index).unwrap() + 10),
        )
        .unwrap();
    }
    trx.commit().unwrap();
    let trx = storage.begin(Mode::ReadOnly).unwrap();
    let find = |query: FindMany| names(&trx.find_many("core.user", &query).unwrap());

    // Bytewise text order; nulls last ascending, first descending; skip and take.
    assert_eq!(
        find(FindMany::default().order_by(Order::asc("username"))),
        ["Alice", "Eve", "bob", "carol", "dave"]
    );
    assert_eq!(
        find(FindMany::default().order_by(Order::asc("username")).skip(1).take(2)),
        ["Eve", "bob"]
    );
    assert_eq!(
        find(
            FindMany::default()
                .order_by(Order::asc("email"))
                .order_by(Order::asc("username"))
        ),
        ["Eve", "bob", "carol", "Alice", "dave"]
    );
    assert_eq!(
        find(
            FindMany::default()
                .order_by(Order::desc("email"))
                .order_by(Order::asc("username"))
        ),
        ["Alice", "dave", "carol", "bob", "Eve"]
    );
    // Filters.
    let sorted = |filter: Where| {
        find(FindMany::filter(filter).order_by(Order::asc("username")))
    };
    assert_eq!(
        sorted(Where::field("username", Cond::Contains("a".into(), Case::Sensitive))),
        ["carol", "dave"]
    );
    assert_eq!(
        sorted(Where::field("username", Cond::Contains("a".into(), Case::Insensitive))),
        ["Alice", "carol", "dave"]
    );
    assert_eq!(
        sorted(Where::field("username", Cond::StartsWith("e".into(), Case::Insensitive))),
        ["Eve"]
    );
    assert_eq!(
        sorted(Where::field("username", Cond::EndsWith("ol".into(), Case::Sensitive))),
        ["carol"]
    );
    assert_eq!(
        sorted(Where::field(
            "username",
            Cond::In(vec![Value::from("bob"), Value::from("Eve"), Value::from("zed")])
        )),
        ["Eve", "bob"]
    );
    assert_eq!(
        sorted(Where::field(
            "username",
            Cond::NotIn(vec![Value::from("bob"), Value::from("Eve")])
        )),
        ["Alice", "carol", "dave"]
    );
    assert_eq!(sorted(Where::field("email", Cond::IsNull(true))), ["Alice", "dave"]);
    // SQL semantics: `not` and a negated comparison skip nulls.
    assert_eq!(
        sorted(Where::field("email", Cond::Not(Value::from("Eve@mail.io")))),
        ["bob", "carol"]
    );
    assert_eq!(
        sorted(Where::Not(Box::new(Where::eq("email", "carol@mail.io")))),
        ["Eve", "bob"]
    );
    assert_eq!(
        sorted(Where::Or(vec![
            Where::eq("username", "bob"),
            Where::field("username", Cond::Gt(Value::from("d"))),
        ])),
        ["bob", "dave"]
    );
    assert_eq!(
        sorted(Where::And(vec![
            Where::field("username", Cond::Gte(Value::from("b"))),
            Where::field("username", Cond::Lt(Value::from("d"))),
        ])),
        ["bob", "carol"]
    );
    assert_eq!(
        trx.count("core.user", Some(&Where::field("email", Cond::IsNull(false))))
            .unwrap(),
        3
    );
    assert!(trx
        .find_many("core.user", &FindMany::filter(Where::eq("nope", 1)))
        .is_err());
    trx.rollback().unwrap();

    // update_many and delete_many.
    let trx = storage.begin(Mode::ReadWrite).unwrap();
    assert_eq!(
        trx.update_many(
            "core.user",
            Some(&Where::field("email", Cond::IsNull(true))),
            Data::from([("status".to_owned(), Value::from("unverified"))]),
        )
        .unwrap(),
        2
    );
    assert_eq!(
        trx.count("core.user", Some(&Where::eq("status", "unverified"))).unwrap(),
        2
    );
    assert_eq!(
        trx.delete_many("core.user", Some(&Where::eq("status", "unverified")))
            .unwrap(),
        2
    );
    assert_eq!(trx.count("core.user", None).unwrap(), 3);
    trx.commit().unwrap();
    wipe(storage, "core.user");
}

fn transactions(storage: &Storage) {
    // A rolled-back transaction leaves nothing.
    let trx = storage.begin(Mode::ReadWrite).unwrap();
    trx.create("core.user", user("ghost", None, 30)).unwrap();
    trx.rollback().unwrap();
    let dropped = storage.begin(Mode::ReadWrite).unwrap();
    dropped.create("core.user", user("ghost", None, 30)).unwrap();
    drop(dropped);
    let check = storage.begin(Mode::ReadOnly).unwrap();
    assert_eq!(check.count("core.user", None).unwrap(), 0);
    check.rollback().unwrap();

    // A value another transaction committed to a unique index is taken; updates
    // read the latest committed revision (read committed, as in Prisma).
    let first = storage.begin(Mode::ReadWrite).unwrap();
    let row = first.create("core.user", user("race", None, 31)).unwrap();
    first.commit().unwrap();
    let second = storage.begin(Mode::ReadWrite).unwrap();
    assert!(matches!(
        second.create("core.user", user("race", None, 32)),
        Err(StorageError::Conflict(_))
    ));
    second.rollback().unwrap();
    let writer = storage.begin(Mode::ReadWrite).unwrap();
    writer
        .update("core.user", &Unique::Id(row.id), Data::from([("status".to_owned(), Value::from("a"))]))
        .unwrap();
    writer.commit().unwrap();
    let writer = storage.begin(Mode::ReadWrite).unwrap();
    let latest = writer
        .update("core.user", &Unique::Id(row.id), Data::from([("status".to_owned(), Value::from("b"))]))
        .unwrap()
        .unwrap();
    assert_eq!(latest.revision, 3);
    writer.commit().unwrap();
    wipe(storage, "core.user");
}

fn relations_and_documents(storage: &Storage) {
    let trx = storage.begin(Mode::ReadWrite).unwrap();
    let owner = trx.create("core.user", user("owner", None, 40)).unwrap();
    let creature = trx
        .create(
            "core.creature",
            Data::from([
                ("username".to_owned(), Value::from("bot")),
                ("creature_type".to_owned(), Value::from("machine")),
                ("public_key".to_owned(), Value::Bytes(vec![41; 8])),
                ("status".to_owned(), Value::from("active")),
                ("owner".to_owned(), Value::Id(owner.id)),
            ]),
        )
        .unwrap();
    let store = trx
        .create(
            "core.store",
            Data::from([
                ("creature".to_owned(), Value::Id(creature.id)),
                ("is_public".to_owned(), Value::Bool(true)),
                ("member_count".to_owned(), Value::Int(3)),
                ("persistent_history".to_owned(), Value::Bool(false)),
                ("signal_count".to_owned(), Value::Int(0)),
            ]),
        )
        .unwrap();
    trx.create(
        "core.store_metadata",
        Data::from([
            ("store".to_owned(), Value::Id(store.id)),
            (
                "document".to_owned(),
                Value::Json(serde_json::json!({"title": "hi", "tags": ["a", 1], "n": 2.5})),
            ),
            ("document_path".to_owned(), Value::from("metadata")),
            ("entry_count".to_owned(), Value::Int(3)),
            ("content_digest".to_owned(), Value::Bytes(vec![0; 32])),
        ]),
    )
    .unwrap();
    trx.commit().unwrap();

    let trx = storage.begin(Mode::ReadOnly).unwrap();
    let stores = trx
        .find_many(
            "core.store",
            &FindMany::filter(Where::eq("creature", creature.id)),
        )
        .unwrap();
    assert_eq!(stores.len(), 1);
    assert_eq!(stores[0].id_of("creature"), Some(creature.id));
    assert_eq!(stores[0].get("is_public"), &Value::Bool(true));
    let metadata = trx
        .find_unique("core.store_metadata", &Unique::fields([("store", store.id)]))
        .unwrap()
        .unwrap();
    assert_eq!(
        metadata.get("document"),
        &Value::Json(serde_json::json!({"title": "hi", "tags": ["a", 1], "n": 2.5}))
    );
    assert_eq!(
        trx.count(
            "core.store",
            Some(&Where::field("member_count", Cond::Gte(Value::Int(3))))
        )
        .unwrap(),
        1
    );
    trx.rollback().unwrap();
    for model in ["core.store_metadata", "core.store", "core.creature", "core.user"] {
        wipe(storage, model);
    }
}

fn migration(storage: &Storage) {
    let trx = storage.begin(Mode::ReadWrite).unwrap();
    let row = trx.create("core.user", user("export", None, 50)).unwrap();
    trx.commit().unwrap();
    let model = storage.schema().model("core.user").unwrap();
    let exported = storage.provider().export(model, None, 100).unwrap();
    assert!(exported.iter().any(|capsule| capsule.id.0 == row.id.0));
    // An identical replay is accepted.
    storage.provider().import(model, &exported).unwrap();
    wipe(storage, "core.user");
}
