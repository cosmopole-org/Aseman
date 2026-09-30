//! The operations, run through the router on a node over the in-memory provider:
//! every family's behavior, the guard of the signed-packet transports with real
//! signatures, and the operation table's agreement with the registry.

use std::sync::Arc;

use aseman_ports::{StoreAccess, StoreDirectory};
use base64::Engine;
use rsa::RsaPrivateKey;
use rsa::pkcs8::{EncodePublicKey, LineEnding};
use rsa::pss::BlindedSigningKey;
use rsa::rand_core::OsRng;
use rsa::sha2::Sha256;
use rsa::signature::{RandomizedSigner, SignatureEncoding};
use serde_json::{Value, json};

use super::dispatch::SignedRequest;
use super::guard::{APPLET_MARKER, Entry, SignedPacket};
use super::{Caller, OperationError, Router};
use crate::node::Node;
use crate::state::StorePermissions;
use crate::state::store_ports::{MembershipPorts, StorePorts};

struct World {
    node: Arc<Node>,
    router: Arc<Router>,
}

impl World {
    fn new() -> Self {
        let node = Node::for_tests();
        let router = node.router();
        Self { node, router }
    }

    fn caller(&self, user_id: &str) -> Caller {
        Caller {
            user_id: user_id.to_owned(),
            store_id: String::new(),
            source: self.node.id(),
        }
    }

    /// Run `path` for `user_id` as the public edge does (already authorized).
    fn run(&self, user_id: &str, path: &str, body: Value) -> Result<Value, OperationError> {
        let operation = self.router.operation(path).expect("a registered operation");
        self.router.execute(
            &self.caller(user_id),
            operation,
            body.to_string().as_bytes(),
            false,
        )
    }

    fn ok(&self, user_id: &str, path: &str, body: Value) -> Value {
        self.run(user_id, path, body)
            .unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    fn refused(&self, user_id: &str, path: &str, body: Value) -> String {
        match self.run(user_id, path, body) {
            Err(OperationError::Refused(message)) => message,
            other => panic!("{path}: expected a refusal, got {other:?}"),
        }
    }

    /// A human creature with a fresh key; its id and key.
    fn human(&self, username: &str) -> (String, RsaPrivateKey) {
        let key = RsaPrivateKey::new(&mut OsRng, 1024).unwrap();
        let public_key = key
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        let created = self.ok(
            "",
            "/creatures/create",
            json!({"type": "human", "username": username, "publicKey": public_key}),
        );
        (created["creature"]["id"].as_str().unwrap().to_owned(), key)
    }

    /// A machine creature owned by `owner`.
    fn machine(&self, owner: &str, username: &str) -> String {
        let key = RsaPrivateKey::new(&mut OsRng, 1024).unwrap();
        let public_key = key
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        let created = self.ok(
            owner,
            "/creatures/create",
            json!({"type": "machine", "username": username, "publicKey": public_key, "ownerId": owner}),
        );
        created["creature"]["id"].as_str().unwrap().to_owned()
    }

    fn store(&self, id: &str, members: &[(&str, StorePermissions)]) {
        self.node
            .in_action(|trx| {
                let stores = StorePorts { trx };
                let owner = members[0].0;
                stores
                    .create_store(
                        &aseman_domain::store::StoreRecord {
                            id: id.to_owned(),
                            persistent_history: true,
                            member_count: 0,
                            ..Default::default()
                        },
                        owner,
                    )
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
                let membership = MembershipPorts { trx };
                for (member, permissions) in members {
                    membership
                        .join(id, member, *permissions)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                }
                Ok(())
            })
            .unwrap();
    }
}

fn sign(key: &RsaPrivateKey, payload: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(
        BlindedSigningKey::<Sha256>::new(key.clone())
            .sign_with_rng(&mut OsRng, payload)
            .to_vec(),
    )
}

#[test]
fn every_registered_surface_has_an_operation_under_both_routes() {
    let world = World::new();
    let shell = aseman_contracts::security::shell_operations().unwrap();
    assert_eq!(shell.len(), 76);
    for (path, registered) in &shell {
        let operation = world.router.operation(path).expect(path);
        assert_eq!(operation.action, registered.action, "{path}");
        let public = world
            .router
            .operation(&format!("/v1/actions{path}"))
            .expect(path);
        assert_eq!(public.path, operation.path);
    }
    assert!(world.router.operation("/creatures/login").is_none());
}

#[test]
fn diagnostics_answer_without_a_caller() {
    let world = World::new();
    assert_eq!(
        world.ok("", "/api/hello", json!({"name": "world"}))["message"],
        "hello world !"
    );
    assert!(
        world.ok("", "/api/time", json!({}))["time"]
            .as_i64()
            .unwrap()
            > 0
    );
    assert!(world.ok("", "/api/ping", json!({})).is_string());
}

#[test]
fn creatures_are_created_read_updated_listed_and_deleted() {
    let world = World::new();
    let (alice, _) = world.human("alice");
    assert!(alice.ends_with("@global"), "a global id: {alice}");

    let read = world.ok(&alice, "/creatures/get", json!({"userId": alice}));
    // A username is qualified with the node that registered it.
    assert_eq!(read["creature"]["username"], "alice@test-node");

    // By username and by fragment, each with its profile fields and defaults.
    let by_name = world.ok(
        &alice,
        "/creatures/getByUsername",
        json!({"username": "alice@test-node"}),
    );
    assert_eq!(by_name["user"]["id"], alice.as_str());
    assert_eq!(by_name["user"]["name"], "Anonymous User");
    let found = world.ok(&alice, "/creatures/find", json!({"username": "lic"}));
    assert_eq!(found["user"]["id"], alice.as_str());

    world.ok(
        &alice,
        "/creatures/update",
        json!({"userId": alice, "username": "alice2"}),
    );
    assert_eq!(
        world.ok(&alice, "/creatures/get", json!({"userId": alice}))["creature"]["username"],
        "alice2@test-node"
    );
    // `meta` reads the metadata; it never writes.
    assert!(
        world
            .ok(&alice, "/creatures/meta", json!({"userId": alice}))
            .is_object()
    );
    assert_eq!(
        world.refused(&alice, "/creatures/meta", json!({"userId": "404@global"})),
        "user not found"
    );

    let (bob, _) = world.human("bob");
    let listed = world.ok(&alice, "/creatures/list", json!({"offset": 0, "count": 10}));
    assert_eq!(listed["creatures"].as_array().unwrap().len(), 2);
    // Another creature may not delete alice.
    world.refused(&bob, "/creatures/delete", json!({"userId": alice}));
    world.ok(&alice, "/creatures/delete", json!({"userId": alice}));
    world.refused(&alice, "/creatures/get", json!({"userId": alice}));

    let types = world.ok(&bob, "/creatures/types", json!({}));
    let names: Vec<&str> = types["types"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|spec| spec["name"].as_str())
        .collect();
    assert!(
        names.contains(&"human") && names.contains(&"machine"),
        "{names:?}"
    );
}

#[test]
fn machines_list_only_machine_creatures() {
    let world = World::new();
    let (alice, _) = world.human("alice");
    let machine = world.machine(&alice, "worker");
    let machines = world.ok(&alice, "/machines/list", json!({"offset": 0, "count": 10}));
    let rows = machines["machines"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], machine.as_str());
    assert_eq!(rows[0]["title"], "untitled");
}

#[test]
fn invalid_input_is_refused_before_anything_runs() {
    let world = World::new();
    match world.run("", "/creatures/get", json!({"userId": 7})) {
        Err(OperationError::Invalid(message)) => assert!(message.contains("invalid input")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn secrets_are_the_owners_and_grants_are_time_boxed() {
    let world = World::new();
    let (alice, _) = world.human("alice");
    let (bob, _) = world.human("bob");
    world.ok(
        &alice,
        "/creatures/secretPut",
        json!({"name": "KEY", "value": "s3cret"}),
    );
    assert_eq!(
        world.ok(&alice, "/creatures/secretGet", json!({"name": "KEY"}))["value"],
        "s3cret"
    );
    // Without a grant another creature is refused.
    assert!(
        world
            .refused(
                &bob,
                "/creatures/secretGet",
                json!({"name": "KEY", "owner": alice})
            )
            .contains("no valid grant")
    );
    world.ok(
        &alice,
        "/creatures/secretGrant",
        json!({"name": "KEY", "grantee": bob, "ttlSeconds": 60}),
    );
    assert_eq!(
        world.ok(
            &bob,
            "/creatures/secretGet",
            json!({"name": "KEY", "owner": alice})
        )["value"],
        "s3cret"
    );
    let granted = world.ok(&bob, "/creatures/secretListGranted", json!({}));
    assert_eq!(granted["grants"][0]["name"], "KEY");
    world.ok(
        &alice,
        "/creatures/secretRevoke",
        json!({"name": "KEY", "grantee": bob}),
    );
    world.refused(
        &bob,
        "/creatures/secretGet",
        json!({"name": "KEY", "owner": alice}),
    );
    assert_eq!(
        world.ok(&alice, "/creatures/secretList", json!({}))["names"],
        json!(["KEY"])
    );
    // A `:` would reach another namespace.
    world.refused(
        &alice,
        "/creatures/secretPut",
        json!({"name": "a:b", "value": "x"}),
    );
}

#[test]
fn stores_signal_members_and_keep_history_by_permission() {
    let world = World::new();
    let (alice, _) = world.human("alice");
    let (bob, _) = world.human("bob");
    let (carol, _) = world.human("carol");
    world.store(
        "s1",
        &[
            (&alice, StorePermissions::owner()),
            (&bob, StorePermissions::member()),
        ],
    );
    let sent = world.ok(
        &alice,
        "/stores/signal",
        json!({"storeId": "s1", "data": "hello", "tags": ["kind=message"]}),
    );
    assert_eq!(sent["persisted"], true);
    let history = world.ok(&bob, "/stores/history", json!({"storeId": "s1"}));
    assert_eq!(history["signals"].as_array().unwrap().len(), 1);
    // A non-member may neither post nor replay.
    world.refused(
        &carol,
        "/stores/signal",
        json!({"storeId": "s1", "data": "x"}),
    );
    world.refused(&carol, "/stores/history", json!({"storeId": "s1"}));
    // Only a manager changes another member's grant.
    world.refused(
        &bob,
        "/stores/setAccess",
        json!({"storeId": "s1", "memberId": alice, "permissions": []}),
    );
    world.ok(
        &alice,
        "/stores/setAccess",
        json!({"storeId": "s1", "memberId": bob, "permissions": ["read"]}),
    );
    let access = world.ok(&bob, "/stores/getAccess", json!({"storeId": "s1"}));
    assert_eq!(
        access["permissions"],
        json!({"read": true, "signal": false, "manage": false})
    );
    world.refused(
        &bob,
        "/stores/signal",
        json!({"storeId": "s1", "data": "x"}),
    );
    assert!(
        world
            .node
            .read(|trx| Ok(MembershipPorts { trx }.is_member("s1", &bob).unwrap()))
            .unwrap()
    );
    assert!(
        world
            .node
            .read(|trx| Ok(StorePorts { trx }.store("s1").unwrap().is_some()))
            .unwrap()
    );
}

#[test]
fn programs_belong_to_their_machines_owner() {
    let world = World::new();
    let (alice, _) = world.human("alice");
    let (bob, _) = world.human("bob");
    let machine = world.machine(&alice, "worker");
    let created = world.ok(
        &alice,
        "/programs/create",
        json!({"appId": machine, "path": "/main", "runtime": "wasm"}),
    );
    let program = created["program"]["id"].as_str().unwrap().to_owned();
    world.refused(
        &bob,
        "/programs/update",
        json!({"programId": program, "path": "/x"}),
    );
    world.ok(
        &alice,
        "/programs/update",
        json!({"programId": program, "path": "/next", "metadata": {"title": "t"}}),
    );
    let listed = world.ok(&alice, "/programs/list", json!({"offset": 0, "count": -1}));
    assert_eq!(listed["machines"][0]["path"], "/next");
    world.refused(&bob, "/programs/delete", json!({"programId": program}));
    world.ok(&alice, "/programs/delete", json!({"programId": program}));
    assert!(
        world.ok(&alice, "/programs/list", json!({"offset": 0, "count": -1}))["machines"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Workload operations need the node's VMM.
    assert!(
        world
            .refused(
                &alice,
                "/programs/deploy",
                json!({"machineId": "404@global"})
            )
            .contains("program does not exist")
    );
}

#[test]
fn token_locks_pay_their_target_on_the_payers_signature() {
    let world = World::new();
    let (payer, payer_key) = world.human("payer");
    let (payee, _) = world.human("payee");
    let accounts = |id: &str| {
        world
            .node
            .read(|trx| crate::state::creature_ports::CreaturePorts { trx }.account_or_empty(id))
            .unwrap()
            .balance
    };
    world
        .node
        .in_action(|trx| {
            let creatures = crate::state::creature_ports::CreaturePorts { trx };
            let mut account = creatures.account_or_empty(&payer)?;
            account.balance = 100;
            creatures.store_account(&account)
        })
        .unwrap();
    world.refused(
        &payer,
        "/creatures/lockToken",
        json!({"type": "pay", "target": payee, "amount": 1000, "unlockAt": 1}),
    );
    let lock = world.ok(
        &payer,
        "/creatures/lockToken",
        json!({"type": "pay", "target": payee, "amount": 40, "unlockAt": 1}),
    );
    let lock_id = lock["tokenId"].as_str().unwrap().to_owned();
    assert_eq!(accounts(&payer), 60);
    let signed = format!("{lock_id}:0:1:40:{payee}");
    let consume = |signature: &str| {
        world.run(
            &payee,
            "/creatures/consumeLock",
            json!({"type": "pay", "userId": payer, "lockId": lock_id, "amount": 40, "signature": signature}),
        )
    };
    assert!(matches!(
        consume("bm90IGEgc2lnbmF0dXJl"),
        Err(OperationError::Refused(_))
    ));
    assert_eq!(
        consume(&sign(&payer_key, signed.as_bytes())).unwrap()["remainingAmount"],
        0
    );
    assert_eq!(accounts(&payee), 40);
    assert!(matches!(
        consume(&sign(&payer_key, signed.as_bytes())),
        Err(OperationError::Refused(_))
    ));
}

#[test]
fn bridge_topics_admit_only_what_the_grant_covers() {
    let world = World::new();
    let token = "bridge-token";
    world
        .node
        .in_action(|trx| {
            crate::state::bridges::put_grant(
                trx,
                &crate::state::bridges::hash_bridge_token(token),
                &json!({"creatureId": "7@global", "topics": ["space:a"], "expiresAt": 0}),
            )
        })
        .unwrap();
    let subscribed = world.ok("", "/gateway/subscribe", json!({"token": token}));
    assert_eq!(subscribed["topics"], json!(["space:a"]));
    world.refused(
        "",
        "/gateway/subscribe",
        json!({"token": token, "topics": ["space:b"]}),
    );
    world.refused("", "/gateway/subscribe", json!({"token": "other"}));
    let published = world.ok(
        "",
        "/gateway/signal",
        json!({"token": token, "topic": "space:a", "action": "crew/message"}),
    );
    assert_eq!(published["creatureId"], "7@global");
    world.refused(
        "",
        "/gateway/signal",
        json!({"token": token, "topic": "space:b", "action": "x"}),
    );
    world.ok("", "/gateway/unsubscribe", json!({"token": token}));
}

#[test]
fn signed_packets_are_admitted_by_their_operations_guard() {
    let world = World::new();
    let (alice, alice_key) = world.human("alice");
    let (bob, _) = world.human("bob");
    let payload = json!({"userId": alice}).to_string();
    let dispatch = |user_id: &str, signature: &str, path: &str, payload: &str, entry: Entry| {
        world.router.dispatch(
            &SignedRequest {
                path,
                packet: SignedPacket {
                    user_id,
                    payload: payload.as_bytes(),
                    signature,
                },
            },
            entry,
        )
    };
    // A user operation needs the creature's own signature over the payload.
    let signature = sign(&alice_key, payload.as_bytes());
    assert_eq!(
        dispatch(
            &alice,
            &signature,
            "/creatures/get",
            &payload,
            Entry::Client
        )
        .unwrap()["creature"]["id"],
        alice.as_str()
    );
    let forged = dispatch(&bob, &signature, "/creatures/get", &payload, Entry::Client).unwrap_err();
    assert_eq!(
        (forged.code(), forged.message()),
        (3, "authorization failed".to_owned())
    );
    // A public operation admits an anonymous packet, not a half-signed one.
    assert!(dispatch("", "", "/api/hello", "{}", Entry::Client).is_ok());
    assert!(dispatch(&alice, "", "/api/hello", "{}", Entry::Client).is_err());
    // Unknown paths and malformed payloads have their own codes.
    assert_eq!(
        dispatch("", "", "/creatures/login", "{}", Entry::Client)
            .unwrap_err()
            .code(),
        1
    );
    assert_eq!(
        dispatch("", "", "/api/hello", "not json", Entry::Client)
            .unwrap_err()
            .code(),
        2
    );
    // A human never authenticates with the applet marker, even from inside.
    assert!(
        dispatch(
            &alice,
            APPLET_MARKER,
            "/creatures/get",
            &payload,
            Entry::Inside
        )
        .is_err()
    );
}

#[test]
fn a_machine_acts_as_itself_from_inside_but_never_moves_value_so() {
    let world = World::new();
    let (alice, _) = world.human("alice");
    let machine = world.machine(&alice, "worker");
    let payload = json!({"userId": machine}).to_string();
    let request = |path: &'static str, payload: &str| {
        world.router.dispatch(
            &SignedRequest {
                path,
                packet: SignedPacket {
                    user_id: &machine,
                    payload: payload.as_bytes(),
                    signature: APPLET_MARKER,
                },
            },
            Entry::Inside,
        )
    };
    assert!(request("/creatures/get", &payload).is_ok());
    // From a client connection the marker is not a signature.
    assert!(
        world
            .router
            .dispatch(
                &SignedRequest {
                    path: "/creatures/get",
                    packet: SignedPacket {
                        user_id: &machine,
                        payload: payload.as_bytes(),
                        signature: APPLET_MARKER,
                    },
                },
                Entry::Client,
            )
            .is_err()
    );
    // A finance operation always needs a real signature.
    let transfer = json!({"toUsername": "alice", "amount": 1}).to_string();
    assert!(request("/creatures/transfer", &transfer).is_err());
}

#[test]
fn a_store_operation_admits_only_members_of_the_store() {
    let world = World::new();
    let (alice, alice_key) = world.human("alice");
    let (bob, bob_key) = world.human("bob");
    world.store("s1", &[(&alice, StorePermissions::owner())]);
    let payload = json!({"storeId": "s1"}).to_string();
    let history = |user: &str, key: &RsaPrivateKey| {
        world.router.dispatch(
            &SignedRequest {
                path: "/stores/history",
                packet: SignedPacket {
                    user_id: user,
                    payload: payload.as_bytes(),
                    signature: &sign(key, payload.as_bytes()),
                },
            },
            Entry::Client,
        )
    };
    assert!(history(&alice, &alice_key).is_ok());
    assert!(history(&bob, &bob_key).is_err());
}

#[test]
fn a_subject_is_the_creature_whose_record_it_names() {
    let world = World::new();
    let (alice, _) = world.human("alice");
    let subject = crate::workloads::vmm::creature_subject(&alice);
    let resolved = world
        .node
        .read(|trx| {
            crate::state::creature_ports::CreaturePorts { trx }
                .legacy_id_of(subject.id)
                .map_err(|error| anyhow::anyhow!("{error}"))
        })
        .unwrap();
    assert_eq!(resolved.as_deref(), Some(alice.as_str()));
    let unknown = world
        .node
        .read(|trx| {
            crate::state::creature_ports::CreaturePorts { trx }
                .legacy_id_of(aseman_domain::Uuid::now_v7())
                .map_err(|error| anyhow::anyhow!("{error}"))
        })
        .unwrap();
    assert_eq!(unknown, None);
}
