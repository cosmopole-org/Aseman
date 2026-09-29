//! Session tokens (ADR 0036): the `core.session_token` model, a minted token
//! naming its user. A token is kept only as its digest, so the store never holds a
//! usable bearer credential. (Sessions from before the migration are revocation
//! markers in `core.session`, ADR 0020; they never authenticate.)

use anyhow::Result;
use aseman_storage::Models;
use aseman_storage::client::core::session_token;
use serde::{Deserialize, Serialize};

use crate::storage::{Trx, failed};

/// The stored key of a token.
fn digest(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"ASEMAN-SESSION-TOKEN-V1\0");
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "userId", default)]
    pub user_id: String,
}

impl Session {
    /// Record the session.
    pub fn save(&self, trx: &Trx) -> Result<()> {
        let key = digest(&self.id);
        trx.session_token()
            .upsert(
                session_token::by_key(key.clone()),
                session_token::Create {
                    key,
                    user_ref: self.user_id.clone(),
                },
                session_token::update().user_ref(self.user_id.clone()),
            )
            .map(drop)
            .map_err(failed)
    }

    /// The session a token names, if any.
    pub fn find(trx: &Trx, token: &str) -> Result<Option<Session>> {
        Ok(trx
            .session_token()
            .find_unique(session_token::by_key(digest(token)))
            .map_err(failed)?
            .map(|row| Session {
                id: token.to_owned(),
                user_id: row.user_ref,
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_resolve_by_token_and_store_only_its_digest() {
        let trx = crate::storage::test_trx();
        Session {
            id: "tok".into(),
            user_id: "1@t".into(),
        }
        .save(&trx)
        .unwrap();
        let found = Session::find(&trx, "tok").unwrap().unwrap();
        assert_eq!((found.id.as_str(), found.user_id.as_str()), ("tok", "1@t"));
        assert!(Session::find(&trx, "other").unwrap().is_none());
        assert!(
            trx.session_token()
                .find_unique(session_token::by_key("tok"))
                .unwrap()
                .is_none()
        );
    }
}
