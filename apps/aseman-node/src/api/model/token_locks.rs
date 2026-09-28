//! Token locks (ADR 0036): a payer's signed payment plan for a target, stored as
//! `core.token_lock` (formerly the `lockedTokens.<id>` member of the payer's
//! `Json::Creature` document), and the markers of consumed tokens.

use anyhow::Result;
use aseman_storage::Models;
use aseman_storage::client::core::{marker, token_lock};
use serde_json::{Map, Value};

use crate::core::trx::{Trx, failed};

fn key(owner: &str, lock_id: &str) -> String {
    format!("{owner}::{lock_id}")
}

/// The lock `lock_id` of `owner`.
pub(crate) fn lock(trx: &Trx, owner: &str, lock_id: &str) -> Result<Option<Map<String, Value>>> {
    Ok(trx
        .token_lock()
        .find_unique(token_lock::by_key(key(owner, lock_id)))
        .map_err(failed)?
        .and_then(|row| row.document.as_object().cloned()))
}

/// Write a lock; with `merge`, deep-merge it into the stored one.
pub(crate) fn put_lock(
    trx: &Trx,
    owner: &str,
    lock_id: &str,
    document: &Map<String, Value>,
    merge: bool,
) -> Result<()> {
    let mut next = if merge {
        lock(trx, owner, lock_id)?.unwrap_or_default()
    } else {
        Map::new()
    };
    aseman_contracts::legacy_documents::merge_legacy_objects(&mut next, document);
    let value = Value::Object(next);
    trx.token_lock()
        .upsert(
            token_lock::by_key(key(owner, lock_id)),
            token_lock::Create {
                key: key(owner, lock_id),
                owner_ref: owner.to_owned(),
                lock_ref: lock_id.to_owned(),
                document: value.clone(),
            },
            token_lock::update().document(value),
        )
        .map(drop)
        .map_err(failed)
}

/// Remove a lock.
pub(crate) fn delete_lock(trx: &Trx, owner: &str, lock_id: &str) -> Result<()> {
    trx.token_lock()
        .delete(token_lock::by_key(key(owner, lock_id)))
        .map(drop)
        .map_err(failed)
}

fn consumed_key(owner: &str, token_id: &str) -> String {
    format!("ConsumedToken::{owner}::{token_id}")
}

/// Whether `owner`'s token was consumed.
pub(crate) fn consumed(trx: &Trx, owner: &str, token_id: &str) -> Result<bool> {
    Ok(trx
        .marker()
        .find_unique(marker::by_key(consumed_key(owner, token_id)))
        .map_err(failed)?
        .is_some_and(|row| row.value == "true"))
}

/// Mark `owner`'s token consumed.
pub(crate) fn mark_consumed(trx: &Trx, owner: &str, token_id: &str) -> Result<()> {
    let key = consumed_key(owner, token_id);
    trx.marker()
        .upsert(
            marker::by_key(key.clone()),
            marker::Create {
                key,
                value: "true".to_owned(),
            },
            marker::update().value("true"),
        )
        .map(drop)
        .map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_merge_read_delete_and_consume() {
        let trx = crate::core::trx::test_trx();
        let mut document = Map::new();
        document.insert("amount".into(), Value::from(5));
        put_lock(&trx, "u1", "l1", &document, true).unwrap();
        let mut more = Map::new();
        more.insert("steps".into(), Value::from(vec![1]));
        put_lock(&trx, "u1", "l1", &more, true).unwrap();
        let stored = lock(&trx, "u1", "l1").unwrap().unwrap();
        assert_eq!((stored["amount"].clone(), stored.len()), (Value::from(5), 2));
        assert!(lock(&trx, "u2", "l1").unwrap().is_none());
        delete_lock(&trx, "u1", "l1").unwrap();
        assert!(lock(&trx, "u1", "l1").unwrap().is_none());
        assert!(!consumed(&trx, "u1", "t").unwrap());
        mark_consumed(&trx, "u1", "t").unwrap();
        assert!(consumed(&trx, "u1", "t").unwrap());
    }
}
