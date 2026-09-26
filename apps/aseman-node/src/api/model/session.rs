//! Translation of `shell/api/model/session.go`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::models::transaction::ITrx;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "userId", default)]
    pub user_id: String,
}

impl Session {
    pub fn type_() -> &'static str {
        "Session"
    }

    pub fn push(&self, trx: &dyn ITrx) {
        let mut cols: HashMap<String, Vec<u8>> = HashMap::new();
        cols.insert("userId".into(), self.user_id.as_bytes().to_vec());
        trx.put_obj(Self::type_(), &self.id, cols);
        trx.put_index(
            Self::type_(),
            "userId",
            "id",
            &self.user_id,
            self.id.as_bytes().to_vec(),
        );
    }

    pub fn pull(mut self, trx: &dyn ITrx) -> Session {
        let m = trx.get_obj(Self::type_(), &self.id);
        if let Some(v) = m.get("userId") {
            self.user_id = String::from_utf8_lossy(v).into_owned();
        }
        self
    }
}
