//! Guest document state (LD-24, ADR 0028): the `putJson`/`getJson`/`getByPrefix`/
//! `delKey`/`getLink` host calls, confined to the calling creature.
//!
//! The caller's creature comes from the node: the packet a runtime or the docker
//! gateway stamped, or the VM context registered for a runtime transaction. The guest's
//! key only ever extends the creature's own prefix:
//! - documents live at `GuestDoc::{creature}::{key}` in the JSON store;
//! - `getLink` reads the creature's own `dbOp` pairs, `{creature}::{key}`.
//!
//! Before this, the calls addressed arbitrary node keys (finance, sessions, secrets,
//! custodial keys). Operations without a trusted creature are refused.

use serde_json::{Map, Value, json};

use crate::models::transaction::ITrx;

/// The guest state operations.
pub(crate) const GUEST_STATE_OPS: [&str; 5] =
    ["putJson", "getJson", "getByPrefix", "delKey", "getLink"];

/// The JSON-store key of a guest document.
pub(crate) fn document_key(creature: &str, key: &str) -> String {
    ["GuestDoc::", creature, "::", key].concat()
}

/// The physical prefix of every record of the creature's documents.
fn records_prefix(creature: &str) -> String {
    ["json::GuestDoc::", creature, "::"].concat()
}

fn required<'a>(input: &'a Value, field: &str) -> Result<&'a str, String> {
    input[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} is required"))
}

/// Run one guest state operation for `creature` inside `trx`.
///
/// # Errors
///
/// A refusal without a trusted creature, a missing field, or an unknown operation.
pub(crate) fn run(
    trx: &dyn ITrx,
    creature: &str,
    op: &str,
    input: &Value,
) -> Result<Value, String> {
    if creature.trim().is_empty() || creature.contains("::") {
        return Err("guest state needs an identified creature".to_owned());
    }
    // On PostgreSQL the creature's own guest database serves it (ADR 0028).
    if let Some(result) = crate::shell::api::model::guest_data::route_state(creature, op, input) {
        return result;
    }
    match op {
        "putJson" => {
            let key = document_key(creature, required(input, "key")?);
            let path = input["path"].as_str().unwrap_or("");
            let merge = input["merge"].as_bool().unwrap_or(true);
            trx.put_json(&key, path, &input["data"], merge)
                .map_err(|error| error.to_string())?;
            Ok(json!({"ok": true}))
        }
        "getJson" => {
            let key = document_key(creature, required(input, "key")?);
            let path = input["path"].as_str().unwrap_or("");
            let data = trx.get_json(&key, path).unwrap_or_else(|_| Map::new());
            Ok(json!({"ok": true, "data": Value::Object(data)}))
        }
        "getByPrefix" => {
            let prefix = input["prefix"].as_str().unwrap_or("");
            let own = records_prefix(creature);
            let keys: Vec<String> = trx
                .get_by_prefix(&[own.as_str(), prefix].concat())
                .into_iter()
                .filter_map(|record| record.strip_prefix(&own).map(str::to_owned))
                .collect();
            Ok(json!({"ok": true, "data": keys}))
        }
        "delKey" => {
            let key = required(input, "key")?;
            let path = input["path"].as_str().unwrap_or("");
            let document = document_key(creature, key);
            if path.is_empty() {
                // The whole document: every record the JSON store splatted for it.
                for record in trx.get_by_prefix(&["json::", &document, "::"].concat()) {
                    trx.del_key(&record);
                }
            } else {
                trx.del_json(&document, path);
            }
            Ok(json!({"ok": true}))
        }
        "getLink" => {
            let key = required(input, "key")?;
            let value = trx.get_link(&[creature, "::", key].concat());
            Ok(json!({"ok": true, "value": value}))
        }
        other => Err(format!("unsupported guest state op: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::actor::model::trx::TrxWrapper;
    use crate::core::actor::model::trx::tests::{StubCore, StubStorage};
    use crate::models::ports::storage::IStorage;
    use std::sync::Arc;

    fn trx() -> Arc<TrxWrapper> {
        let storage: Arc<dyn IStorage> = StubStorage::new();
        TrxWrapper::new(
            Arc::new(StubCore {
                storage: storage.clone(),
            }),
            storage,
            false,
        )
    }

    #[test]
    fn guests_reach_only_their_own_documents_and_pairs() {
        let trx = trx();
        // Node state a guest must never reach.
        trx.put_json(
            "CreatMeta::7@global",
            "metadata",
            &json!({"secret": 1}),
            true,
        )
        .unwrap();
        trx.put_link("UserPrivateKey::7@global", "-----BEGIN PRIVATE KEY-----");
        trx.put_link("8@global::own", "mine");

        let alice = "8@global";
        run(
            &*trx,
            alice,
            "putJson",
            &json!({"key": "counter", "path": "doc", "data": {"n": 1}}),
        )
        .unwrap();
        assert_eq!(
            run(
                &*trx,
                alice,
                "getJson",
                &json!({"key": "counter", "path": "doc"})
            )
            .unwrap()["data"],
            json!({"n": 1})
        );
        // Node keys, and other creatures' documents, are out of reach.
        for key in ["../CreatMeta::7@global", "CreatMeta::7@global"] {
            assert_eq!(
                run(
                    &*trx,
                    alice,
                    "getJson",
                    &json!({"key": key, "path": "metadata"})
                )
                .unwrap()["data"],
                json!({})
            );
        }
        assert_eq!(
            run(
                &*trx,
                "9@global",
                "getJson",
                &json!({"key": "counter", "path": "doc"})
            )
            .unwrap()["data"],
            json!({})
        );
        assert_eq!(
            run(
                &*trx,
                alice,
                "getLink",
                &json!({"key": "UserPrivateKey::7@global"})
            )
            .unwrap()["value"],
            ""
        );
        assert_eq!(
            run(&*trx, alice, "getLink", &json!({"key": "own"})).unwrap()["value"],
            "mine"
        );
        // Listing sees only the creature's own records, in the guest's key space.
        let listed = run(&*trx, alice, "getByPrefix", &json!({"prefix": ""})).unwrap();
        assert_eq!(listed["data"], json!(["counter::doc", "counter::doc.n"]));
        assert_eq!(
            run(&*trx, "9@global", "getByPrefix", &json!({"prefix": ""})).unwrap()["data"],
            json!([])
        );
        // Deleting a whole document removes every record of it, and nothing else.
        run(&*trx, alice, "delKey", &json!({"key": "counter"})).unwrap();
        assert_eq!(
            run(&*trx, alice, "getByPrefix", &json!({"prefix": ""})).unwrap()["data"],
            json!([])
        );
        assert!(trx.get_json("CreatMeta::7@global", "metadata").is_ok());
        // Without a trusted creature, nothing runs.
        for creature in ["", "  ", "a::b"] {
            assert!(run(&*trx, creature, "getJson", &json!({"key": "counter"})).is_err());
        }
    }
}
