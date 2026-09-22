use super::*;
use serde_json::json;
use std::collections::BTreeMap;

/// Programs `p-alice`/`p-alice-2` and machine `m-alice` belong to user `alice`;
/// `p-bob` and `m-bob` to `bob`. Alice manages store `s-alice`; bob only reads it.
/// VM `vm-alice-7` was launched by `p-alice-2`, `vm-bob-1` by `p-bob`.
struct World;

impl AuthorityLookups for World {
    fn owner_user(&self, id: &str) -> String {
        match id {
            "p-alice" | "p-alice-2" | "m-alice" => "alice",
            "p-bob" | "m-bob" => "bob",
            _ => "",
        }
        .to_owned()
    }
    fn vm_program(&self, vm: &str) -> String {
        match vm {
            "vm-alice-7" => "p-alice-2",
            "vm-bob-1" => "p-bob",
            _ => "",
        }
        .to_owned()
    }
    fn store_permissions(&self, store: &str, member: &str) -> (bool, bool, bool) {
        BTreeMap::from([
            (("s-alice", "alice"), (true, true, true)),
            (("s-alice", "bob"), (true, false, false)),
        ])
        .get(&(store, member))
        .copied()
        .unwrap_or_default()
    }
    fn resource_store_machine(&self, store: &str) -> String {
        match store {
            "rs-alice" => "m-alice",
            "rs-bob" => "m-bob",
            _ => "",
        }
        .to_owned()
    }
    fn is_human(&self, id: &str) -> bool {
        matches!(id, "alice" | "bob" | LEGACY_ROOT)
    }
}

fn guest(op: &str, program: &str, mut input: JsonValue) -> Result<JsonValue, String> {
    let caller = host_caller(&World, &format!("vm-{program}"), program, program);
    authorize_host_call(&World, op, &caller, &mut input, 1_800_000_000_000)?;
    Ok(input)
}

fn signed(path: &str, user: &str, input: JsonValue) -> Result<(), String> {
    authorize_shell_action(&World, path, user, &input, 1_800_000_000_000)
}

#[test]
fn guest_creature_crud_is_limited_to_the_callers_owner() {
    assert!(guest("updateCreature", "p-alice", json!({"id": "m-alice"})).is_ok());
    assert!(guest("deleteCreature", "p-alice", json!({"id": "p-alice-2"})).is_ok());
    for op in ["updateCreature", "deleteCreature", "removeOwnedCreature"] {
        assert!(
            guest(op, "p-alice", json!({"id": "m-bob"})).is_err(),
            "{op}"
        );
    }
    assert!(guest("updateCreature", "p-alice", json!({})).is_err());
    assert!(guest("getCreature", "p-alice", json!({"id": "m-bob"})).is_ok());
    let created = guest("createCreature", "p-alice", json!({"id": "new"})).unwrap();
    assert_eq!(created["ownerId"], "alice");
    assert!(guest(
        "createCreature",
        "p-alice",
        json!({"id": "x", "balance": 100})
    )
    .is_err());
    assert!(guest(
        "createCreature",
        "p-alice",
        json!({"id": "x", "ownerId": "bob"})
    )
    .is_err());
    assert!(guest("createCreature", "p-orphan", json!({"id": "x"})).is_err());
}

#[test]
fn guest_stores_and_access_follow_membership() {
    assert!(guest("updateStore", "p-alice", json!({"storeId": "s-alice"})).is_ok());
    assert!(guest("createAccess", "p-alice", json!({"storeId": "s-alice"})).is_ok());
    assert!(guest("deleteStore", "p-alice", json!({"storeId": "s-alice"})).is_ok());
    assert!(guest("getStore", "p-bob", json!({"storeId": "s-alice"})).is_ok());
    for op in [
        "updateStore",
        "deleteStore",
        "createAccess",
        "removeAccess",
        "signal",
    ] {
        assert!(
            guest(op, "p-bob", json!({"storeId": "s-alice"})).is_err(),
            "{op}"
        );
    }
    assert!(guest("signal", "p-alice", json!({"storeId": "s-alice"})).is_ok());
    assert!(guest("createStore", "p-bob", json!({"storeId": "fresh"})).is_ok());
}

#[test]
fn guest_programs_workloads_and_resource_stores_follow_ownership() {
    assert!(guest(
        "updateProgram",
        "p-alice",
        json!({"programId": "p-alice-2"})
    )
    .is_ok());
    assert!(guest("deleteProgram", "p-alice", json!({"programId": "p-bob"})).is_err());
    assert!(guest("deployEntity", "p-bob", json!({"programId": "p-alice"})).is_err());
    // A sibling program's VM is the owner's; another owner's VM is not.
    assert!(guest("terminateVm", "p-alice", json!({"vmId": "vm-alice-7"})).is_ok());
    assert!(guest("execVm", "p-alice", json!({"vmId": "vm-bob-1"})).is_err());
    assert!(guest(
        "updateResourceStore",
        "p-alice",
        json!({"storeId": "rs-alice"})
    )
    .is_ok());
    assert!(guest(
        "deleteResourceStore",
        "p-alice",
        json!({"storeId": "rs-bob"})
    )
    .is_err());
    assert!(guest(
        "createResourceEntity",
        "p-bob",
        json!({"storeId": "rs-alice"})
    )
    .is_err());
    assert!(guest("createResourceStore", "p-bob", json!({"storeId": "rs-new"})).is_ok());
}

#[test]
fn guest_data_is_served_and_unknown_or_removed_calls_are_refused() {
    for op in [
        "dbOp",
        "putJson",
        "getJson",
        "getByPrefix",
        "delKey",
        "getLink",
        "commitTrx",
    ] {
        assert!(guest(op, "p-alice", json!({"key": "k"})).is_ok(), "{op}");
    }
    assert_eq!(
        guest("teleport", "p-alice", json!({})),
        Err("unregistered surface unified-host-call teleport".to_owned())
    );
    assert!(guest("protocolApi", "p-alice", json!({})).is_err());
    // Outbound HTTP needs a grant, but is shadowed until A406 grants exist.
    assert!(guest(
        "httpRequest",
        "p-alice",
        json!({"url": "https://api.example"})
    )
    .is_ok());
}

#[test]
fn signed_actions_are_authorized_for_their_signer() {
    // Public actions, and anonymous callers only there.
    assert!(signed("/api/ping", "", json!({})).is_ok());
    assert!(signed("/creatures/create", "", json!({})).is_ok());
    assert!(signed("/creatures/get", "", json!({"id": "m-bob"})).is_err());
    // A user acts on their own creatures, not another user's.
    assert!(signed("/creatures/update", "alice", json!({"id": "m-alice"})).is_ok());
    assert!(signed("/creatures/delete", "alice", json!({"id": "m-bob"})).is_err());
    assert!(signed("/creatures/delete", "alice", json!({"id": "alice"})).is_ok());
    // Stores by membership.
    assert!(signed("/stores/signal", "alice", json!({"storeId": "s-alice"})).is_ok());
    assert!(signed("/stores/signal", "bob", json!({"storeId": "s-alice"})).is_err());
    assert!(signed("/stores/history", "bob", json!({"storeId": "s-alice"})).is_ok());
    assert!(signed("/stores/setAccess", "bob", json!({"storeId": "s-alice"})).is_err());
    // Programs by their machine's owner.
    assert!(signed("/programs/update", "alice", json!({"programId": "p-alice"})).is_ok());
    assert!(signed("/programs/delete", "alice", json!({"programId": "p-bob"})).is_err());
    // Minting is the root's alone.
    assert!(signed("/creatures/mint", LEGACY_ROOT, json!({"amount": 1})).is_ok());
    assert!(signed("/creatures/mint", "alice", json!({"amount": 1})).is_err());
    // Custodial email login is shadowed until RL-019 removes it.
    assert!(signed("/creatures/login", "", json!({})).is_ok());
    assert!(signed("/nowhere", "alice", json!({})).is_err());
}

#[test]
fn requests_name_their_resource_so_grants_match_only_it() {
    assert_eq!(
        target_id(
            "network",
            &json!({"url": "https://api.example:8443/v1/chat"})
        ),
        "api.example"
    );
    assert_eq!(target_id("network", &json!({"url": "not a url"})), "");
    assert_eq!(target_id("store", &json!({"storeId": "s1"})), "s1");
    assert_eq!(target_id("program", &json!({"programId": "p1"})), "p1");
    assert_eq!(target_id("guest_data", &json!({"key": "k"})), "");
}
