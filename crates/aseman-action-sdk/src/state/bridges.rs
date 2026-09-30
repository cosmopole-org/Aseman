//! Gateway bridge grants and topic claims (ADR 0036): `core.bridge_grant` holds a
//! grant keyed by its token's SHA-256 digest, `core.bridge_topic` the one creature
//! that owns a topic.

use anyhow::{Result, anyhow};
use aseman_storage::Models;
use aseman_storage::client::core::{bridge_grant, bridge_topic};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::state::failed;
use crate::util::Trx;

/// Where the grant document sits in its JSON record.
const DOCUMENT_PATH: &str = "grant";

/// Hash a bearer token the way grants are keyed.
pub fn hash_bridge_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.trim().as_bytes());
    hex::encode(hasher.finalize())
}

fn digest(token_hash: &str) -> Result<Vec<u8>> {
    hex::decode(token_hash)
        .ok()
        .filter(|digest| digest.len() == 32)
        .ok_or_else(|| anyhow!("bridge token hash is not a SHA-256 hex digest"))
}

/// The creature owning `topic`, if claimed.
pub fn topic_owner(trx: &Trx, topic: &str) -> Result<Option<String>> {
    Ok(trx
        .bridge_topic()
        .find_unique(bridge_topic::by_topic(topic))
        .map_err(failed)?
        .map(|row| row.owner_ref))
}

/// Claim `topic` for `owner` (the caller has checked it may).
pub fn claim_topic(trx: &Trx, topic: &str, owner: &str) -> Result<()> {
    trx.bridge_topic()
        .upsert(
            bridge_topic::by_topic(topic),
            bridge_topic::Create {
                topic: topic.to_owned(),
                owner_ref: owner.to_owned(),
            },
            bridge_topic::update().owner_ref(owner),
        )
        .map(drop)
        .map_err(failed)
}

/// Store the grant minted for the token whose hash is `token_hash`.
pub fn put_grant(trx: &Trx, token_hash: &str, grant: &Value) -> Result<()> {
    let token_digest = digest(token_hash)?;
    let minted_by = grant["creatureId"].as_str().unwrap_or_default().to_owned();
    let expires_at_micros = grant["expiresAt"]
        .as_i64()
        .unwrap_or(0)
        .saturating_mul(1_000);
    let entry_count = grant
        .as_object()
        .map_or(0, |fields| i64::try_from(fields.len()).unwrap_or(i64::MAX));
    let content_digest = Sha256::digest(serde_json::to_vec(grant)?).to_vec();
    trx.bridge_grant()
        .upsert(
            bridge_grant::by_token_digest(token_digest.clone()),
            bridge_grant::Create {
                token_digest,
                minted_by_ref: minted_by.clone(),
                expires_at_micros,
                document: grant.clone(),
                document_path: DOCUMENT_PATH.to_owned(),
                entry_count,
                content_digest: content_digest.clone(),
            },
            bridge_grant::update()
                .minted_by_ref(minted_by)
                .expires_at_micros(expires_at_micros)
                .document(grant.clone())
                .entry_count(entry_count)
                .content_digest(content_digest),
        )
        .map(drop)
        .map_err(failed)
}

/// The grant document for `token_hash`, if one was minted.
pub fn grant(trx: &Trx, token_hash: &str) -> Result<Option<Value>> {
    let Ok(token_digest) = digest(token_hash) else {
        return Ok(None);
    };
    Ok(trx
        .bridge_grant()
        .find_unique(bridge_grant::by_token_digest(token_digest))
        .map_err(failed)?
        .map(|row| row.document))
}

/// Drop the grant for `token_hash`.
pub fn delete_grant(trx: &Trx, token_hash: &str) -> Result<()> {
    let Ok(token_digest) = digest(token_hash) else {
        return Ok(());
    };
    trx.bridge_grant()
        .delete(bridge_grant::by_token_digest(token_digest))
        .map(drop)
        .map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_and_topics_round_trip() {
        let trx = crate::state::test_trx();
        let hash = hex::encode([7u8; 32]);
        let grant_document =
            serde_json::json!({"creatureId": "c1", "topics": ["t"], "expiresAt": 5});
        put_grant(&trx, &hash, &grant_document).unwrap();
        assert_eq!(grant(&trx, &hash).unwrap(), Some(grant_document));
        delete_grant(&trx, &hash).unwrap();
        assert_eq!(grant(&trx, &hash).unwrap(), None);
        assert_eq!(grant(&trx, "not-hex").unwrap(), None);

        assert_eq!(topic_owner(&trx, "t").unwrap(), None);
        claim_topic(&trx, "t", "c1").unwrap();
        assert_eq!(topic_owner(&trx, "t").unwrap().as_deref(), Some("c1"));
    }
}