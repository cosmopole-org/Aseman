//! Legacy session tokens (ADR 0036): the `core.session_token` model, a minted token
//! naming its user.

use anyhow::Result;
use aseman_storage::Models;
use aseman_storage::client::core::session_token;
use serde::{Deserialize, Serialize};

use crate::core::trx::{Trx, failed};

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
        trx.session_token()
            .upsert(
                session_token::by_key(self.id.clone()),
                session_token::Create {
                    key: self.id.clone(),
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
            .find_unique(session_token::by_key(token))
            .map_err(failed)?
            .map(|row| Session {
                id: row.key,
                user_id: row.user_ref,
            }))
    }
}
