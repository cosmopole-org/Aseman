//! Creature-owned secrets (ADR 0036): `core.secret_value` holds an owner's
//! encrypted secret, `core.secret_access` a time-boxed grant to another creature.
//! Access control stays with the callers; this module only stores.

use anyhow::Result;
use aseman_storage::client::core::{secret_access, secret_value};
use aseman_storage::{FindMany, Models};

use crate::core::trx::{Trx, failed};

fn value_key(owner: &str, name: &str) -> String {
    format!("{owner}::{name}")
}

fn grant_key(owner: &str, name: &str, grantee: &str) -> String {
    format!("{owner}::{name}::{grantee}")
}

/// The encrypted secret `name` of `owner`, if stored.
pub(crate) fn blob(trx: &Trx, owner: &str, name: &str) -> Result<Option<String>> {
    Ok(trx
        .secret_value()
        .find_unique(secret_value::by_key(value_key(owner, name)))
        .map_err(failed)?
        .map(|row| row.blob))
}

/// Store (or replace) an encrypted secret.
pub(crate) fn put_blob(trx: &Trx, owner: &str, name: &str, blob: &str) -> Result<()> {
    let key = value_key(owner, name);
    trx.secret_value()
        .upsert(
            secret_value::by_key(key.clone()),
            secret_value::Create {
                key,
                owner_ref: owner.to_owned(),
                name: name.to_owned(),
                blob: blob.to_owned(),
            },
            secret_value::update().blob(blob),
        )
        .map(drop)
        .map_err(failed)
}

/// The names of `owner`'s secrets, in name order.
pub(crate) fn names(trx: &Trx, owner: &str) -> Result<Vec<String>> {
    Ok(trx
        .secret_value()
        .find_many(
            FindMany::filter(secret_value::owner_ref().eq(owner))
                .order_by(secret_value::name().asc()),
        )
        .map_err(failed)?
        .into_iter()
        .map(|row| row.name)
        .collect())
}

/// When `grantee`'s access to `owner`'s secret `name` expires (0: no grant).
pub(crate) fn grant_expiry(trx: &Trx, owner: &str, name: &str, grantee: &str) -> Result<i64> {
    Ok(trx
        .secret_access()
        .find_unique(secret_access::by_key(grant_key(owner, name, grantee)))
        .map_err(failed)?
        .map_or(0, |row| row.expires_at_millis))
}

/// Grant `grantee` access until `expires_at_millis`.
pub(crate) fn grant(
    trx: &Trx,
    owner: &str,
    name: &str,
    grantee: &str,
    expires_at_millis: i64,
) -> Result<()> {
    let key = grant_key(owner, name, grantee);
    trx.secret_access()
        .upsert(
            secret_access::by_key(key.clone()),
            secret_access::Create {
                key,
                owner_ref: owner.to_owned(),
                name: name.to_owned(),
                grantee_ref: grantee.to_owned(),
                expires_at_millis,
            },
            secret_access::update().expires_at_millis(expires_at_millis),
        )
        .map(drop)
        .map_err(failed)
}

pub(crate) fn revoke(trx: &Trx, owner: &str, name: &str, grantee: &str) -> Result<()> {
    trx.secret_access()
        .delete(secret_access::by_key(grant_key(owner, name, grantee)))
        .map(drop)
        .map_err(failed)
}

/// The unexpired grants `grantee` holds: `(owner, name, expires_at_millis)`.
pub(crate) fn grants_of(trx: &Trx, grantee: &str, now_millis: i64) -> Result<Vec<(String, String, i64)>> {
    Ok(trx
        .secret_access()
        .find_many(
            FindMany::filter(
                secret_access::grantee_ref()
                    .eq(grantee)
                    .and(secret_access::expires_at_millis().gt(now_millis)),
            )
            .order_by(secret_access::owner_ref().asc())
            .order_by(secret_access::name().asc()),
        )
        .map_err(failed)?
        .into_iter()
        .map(|row| (row.owner_ref, row.name, row.expires_at_millis))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_and_grants_round_trip() {
        let trx = crate::core::trx::test_trx();
        put_blob(&trx, "o", "b", "x").unwrap();
        put_blob(&trx, "o", "a", "y").unwrap();
        assert_eq!(names(&trx, "o").unwrap(), ["a", "b"]);
        assert_eq!(blob(&trx, "o", "a").unwrap().as_deref(), Some("y"));
        grant(&trx, "o", "a", "g", 100).unwrap();
        grant(&trx, "o", "b", "g", 10).unwrap();
        assert_eq!(grant_expiry(&trx, "o", "a", "g").unwrap(), 100);
        assert_eq!(grants_of(&trx, "g", 50).unwrap(), [("o".into(), "a".into(), 100)]);
        revoke(&trx, "o", "a", "g").unwrap();
        assert_eq!(grant_expiry(&trx, "o", "a", "g").unwrap(), 0);
    }
}
