//! Token locks (ADR 0036): a payer's signed payment plan for a target, one
//! `core.token_lock` per `(owner, lock)` — formerly the `lockedTokens.{lock}` member of
//! the payer's `Json::Creature` document.

use aseman_contracts::documents::merge_objects;
use aseman_storage::client::core::token_lock;
use aseman_storage::{Models, StorageResult, Trx};
use serde_json::{Map, Value};

/// The legacy document path of a lock: `lockedTokens.{lock}`.
pub const LOCKED_TOKENS: &str = "lockedTokens";

fn key(owner: &str, lock_id: &str) -> String {
    format!("{owner}::{lock_id}")
}

/// The lock `lock_id` of `owner`.
pub fn lock(trx: &Trx, owner: &str, lock_id: &str) -> StorageResult<Option<Map<String, Value>>> {
    Ok(trx
        .token_lock()
        .find_unique(token_lock::by_key(key(owner, lock_id)))?
        .and_then(|row| row.document.as_object().cloned()))
}

/// Write a lock; with `merge`, deep-merge it into the stored one.
pub fn put_lock(
    trx: &Trx,
    owner: &str,
    lock_id: &str,
    document: &Map<String, Value>,
    merge: bool,
) -> StorageResult<()> {
    let mut next = if merge {
        lock(trx, owner, lock_id)?.unwrap_or_default()
    } else {
        Map::new()
    };
    merge_objects(&mut next, document);
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
}

/// Remove a lock.
pub fn delete_lock(trx: &Trx, owner: &str, lock_id: &str) -> StorageResult<()> {
    trx.token_lock()
        .delete(token_lock::by_key(key(owner, lock_id)))
        .map(drop)
}
