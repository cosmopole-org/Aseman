//! Token locks (ADR 0036): a payer's signed payment plan for a target, stored as
//! `core.token_lock` by `aseman_capsule::token_lock` (formerly the `lockedTokens.<id>`
//! member of the payer's `Json::Creature` document).

use anyhow::Result;
use aseman_capsule::token_lock;
use serde_json::{Map, Value};

use crate::storage::{Trx, failed};

/// The lock `lock_id` of `owner`.
pub(crate) fn lock(trx: &Trx, owner: &str, lock_id: &str) -> Result<Option<Map<String, Value>>> {
    token_lock::lock(trx, owner, lock_id).map_err(failed)
}

/// Write a lock; with `merge`, deep-merge it into the stored one.
pub(crate) fn put_lock(
    trx: &Trx,
    owner: &str,
    lock_id: &str,
    document: &Map<String, Value>,
    merge: bool,
) -> Result<()> {
    token_lock::put_lock(trx, owner, lock_id, document, merge).map_err(failed)
}

/// Remove a lock.
pub(crate) fn delete_lock(trx: &Trx, owner: &str, lock_id: &str) -> Result<()> {
    token_lock::delete_lock(trx, owner, lock_id).map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_merge_read_and_delete() {
        let trx = crate::storage::test_trx();
        let mut document = Map::new();
        document.insert("amount".into(), Value::from(5));
        put_lock(&trx, "u1", "l1", &document, true).unwrap();
        let mut more = Map::new();
        more.insert("steps".into(), Value::from(vec![1]));
        put_lock(&trx, "u1", "l1", &more, true).unwrap();
        let stored = lock(&trx, "u1", "l1").unwrap().unwrap();
        assert_eq!(
            (stored["amount"].clone(), stored.len()),
            (Value::from(5), 2)
        );
        assert!(lock(&trx, "u2", "l1").unwrap().is_none());
        delete_lock(&trx, "u1", "l1").unwrap();
        assert!(lock(&trx, "u1", "l1").unwrap().is_none());
    }
}
