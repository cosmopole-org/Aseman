//! Creature-owned secrets (ADR 0023, ADR 0036): `core.creature_secret` holds an
//! owner's authenticated ciphertext, `core.secret_grant` a time-boxed grant to another
//! creature — the same records `storage migrate` writes. Access control stays
//! with the callers; this module only stores.

use anyhow::{Result, anyhow};
use aseman_storage::client::core::{creature, creature_secret, legacy_identity, secret_grant};
use aseman_storage::{FindMany, Id, Models};
use base64::Engine as _;
use serde_json::Value;

use crate::state::failed;
use crate::util::{Trx, secret_crypto};

/// The algorithm of the node's secret blobs (`secret_crypto`).
pub const SECRET_ALGORITHM: &str = "chacha20poly1305-legacy-v1";

fn creature_id(creature: &str) -> Id {
    Id::for_key("Creature", creature)
}

fn secret(trx: &Trx, owner: &str, name: &str) -> Result<Option<creature_secret::CreatureSecret>> {
    trx.creature_secret()
        .find_unique(creature_secret::by_creature_and_name(
            creature_id(owner),
            name,
        ))
        .map_err(failed)
}

/// The encrypted secret `name` of `owner` (base64 `nonce || ciphertext || tag`).
pub fn blob(trx: &Trx, owner: &str, name: &str) -> Result<Option<String>> {
    Ok(secret(trx, owner, name)?
        .map(|secret| base64::engine::general_purpose::STANDARD.encode(secret.ciphertext)))
}

/// Store (or replace) `owner`'s secret `name`, encrypted under `master_key`.
pub fn put_blob(
    trx: &Trx,
    owner: &str,
    name: &str,
    blob: &str,
    master_key: &[u8; 32],
) -> Result<()> {
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(blob)
        .map_err(|_| anyhow!("secret blob is not base64"))?;
    let fingerprint = secret_crypto::fingerprint(master_key).to_vec();
    trx.creature_secret()
        .upsert(
            creature_secret::by_creature_and_name(creature_id(owner), name),
            creature_secret::Create {
                name: name.to_owned(),
                algorithm: SECRET_ALGORITHM.to_owned(),
                ciphertext: ciphertext.clone(),
                key_fingerprint: fingerprint.clone(),
                creature: Some(creature_id(owner)),
            },
            creature_secret::update()
                .algorithm(SECRET_ALGORITHM)
                .ciphertext(ciphertext)
                .key_fingerprint(fingerprint),
        )
        .map(drop)
        .map_err(failed)
}

/// The names of `owner`'s secrets, in name order.
pub fn names(trx: &Trx, owner: &str) -> Result<Vec<String>> {
    Ok(trx
        .creature_secret()
        .find_many(
            FindMany::filter(creature_secret::creature().eq(creature_id(owner)))
                .order_by(creature_secret::name().asc()),
        )
        .map_err(failed)?
        .into_iter()
        .map(|secret| secret.name)
        .collect())
}

/// When `grantee`'s access to `owner`'s secret `name` expires, in Unix milliseconds
/// (0: no grant).
pub fn grant_expiry(trx: &Trx, owner: &str, name: &str, grantee: &str) -> Result<i64> {
    let Some(secret) = secret(trx, owner, name)? else {
        return Ok(0);
    };
    Ok(trx
        .secret_grant()
        .find_unique(secret_grant::by_secret_and_grantee_ref(secret.id, grantee))
        .map_err(failed)?
        .map_or(0, |grant| grant.expires_at_micros / 1_000))
}

/// Grant `grantee` access until `expires_at_millis`. The secret must exist.
pub fn grant(
    trx: &Trx,
    owner: &str,
    name: &str,
    grantee: &str,
    expires_at_millis: i64,
) -> Result<()> {
    let secret = secret(trx, owner, name)?.ok_or_else(|| anyhow!("secret not found"))?;
    let expires_at_micros = expires_at_millis.saturating_mul(1_000);
    // A grantee that is a creature is related; any other grantee is only named.
    let grantee_id = creature_id(grantee);
    let related = trx
        .creature()
        .find_unique(creature::by_id(grantee_id))
        .map_err(failed)?
        .map(|_| grantee_id);
    trx.secret_grant()
        .upsert(
            secret_grant::by_secret_and_grantee_ref(secret.id, grantee),
            secret_grant::Create {
                grantee_ref: grantee.to_owned(),
                expires_at_micros,
                secret: Some(secret.id),
                grantee: related,
            },
            secret_grant::update().expires_at_micros(expires_at_micros),
        )
        .map(drop)
        .map_err(failed)
}

/// Revoke `grantee`'s access to `owner`'s secret `name`.
pub fn revoke(trx: &Trx, owner: &str, name: &str, grantee: &str) -> Result<()> {
    let Some(secret) = secret(trx, owner, name)? else {
        return Ok(());
    };
    trx.secret_grant()
        .delete(secret_grant::by_secret_and_grantee_ref(secret.id, grantee))
        .map(drop)
        .map_err(failed)
}

/// The unexpired grants `grantee` holds: `(owner, name, expires_at_millis)`, in
/// owner and name order.
pub fn grants_of(
    trx: &Trx,
    grantee: &str,
    now_millis: i64,
) -> Result<Vec<(String, String, i64)>> {
    let mut grants = Vec::new();
    for grant in trx
        .secret_grant()
        .find_many(FindMany::filter(
            secret_grant::grantee_ref()
                .eq(grantee)
                .and(secret_grant::expires_at_micros().gt(now_millis.saturating_mul(1_000))),
        ))
        .map_err(failed)?
    {
        let Some(secret) = grant
            .secret
            .map(|id| {
                trx.creature_secret()
                    .find_unique(creature_secret::by_id(id))
            })
            .transpose()
            .map_err(failed)?
            .flatten()
        else {
            continue;
        };
        let Some(owner) =
            secret
                .creature
                .map(|id| {
                    trx.legacy_identity().find_unique(
                        legacy_identity::by_target_kind_and_target_id(creature::NAME, id),
                    )
                })
                .transpose()
                .map_err(failed)?
                .flatten()
        else {
            continue;
        };
        grants.push((
            owner.legacy_id,
            secret.name,
            grant.expires_at_micros / 1_000,
        ));
    }
    grants.sort();
    Ok(grants)
}

/// The unexpired `{owner, name, expiresAt}` grants held by `grantee`, shared
/// between the `/creatures/secretListGranted` operation and the guest host call
/// so both return the same set.
pub fn granted_to(trx: &Trx, grantee: &str) -> Result<Vec<Value>> {
    let now = chrono::Utc::now().timestamp_millis();
    Ok(grants_of(trx, grantee, now)?
        .into_iter()
        .map(|(owner, name, expires_at)| {
            serde_json::json!({ "owner": owner, "name": name, "expiresAt": expires_at })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_and_grants_round_trip() {
        let trx = crate::state::test_trx();
        crate::state::seed_humans(&trx, &["1@t", "2@t"]);
        let key = [7; 32];
        let sealed = |value: &str| secret_crypto::encrypt(value.as_bytes(), &key).unwrap();
        let (x, y) = (sealed("x"), sealed("y"));
        put_blob(&trx, "1@t", "b", &x, &key).unwrap();
        put_blob(&trx, "1@t", "a", &y, &key).unwrap();
        assert_eq!(names(&trx, "1@t").unwrap(), ["a", "b"]);
        assert_eq!(blob(&trx, "1@t", "a").unwrap(), Some(y));
        assert!(grant(&trx, "1@t", "missing", "2@t", 1).is_err());
        grant(&trx, "1@t", "a", "2@t", 100).unwrap();
        grant(&trx, "1@t", "b", "2@t", 10).unwrap();
        assert_eq!(grant_expiry(&trx, "1@t", "a", "2@t").unwrap(), 100);
        assert_eq!(
            grants_of(&trx, "2@t", 50).unwrap(),
            [("1@t".into(), "a".into(), 100)]
        );
        revoke(&trx, "1@t", "a", "2@t").unwrap();
        assert_eq!(grant_expiry(&trx, "1@t", "a", "2@t").unwrap(), 0);
    }
}