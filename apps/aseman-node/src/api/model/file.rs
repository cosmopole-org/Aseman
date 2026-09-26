//! Translation of `shell/api/model/file.go`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::models::transaction::ITrx;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct File {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "storeId", default)]
    pub store_id: String,
    #[serde(rename = "senderId", default)]
    pub owner_id: String,
}

impl File {
    pub fn type_() -> &'static str {
        "File"
    }

    pub fn push(&self, trx: &dyn ITrx) {
        let mut cols: HashMap<String, Vec<u8>> = HashMap::new();
        cols.insert("storeId".into(), self.store_id.as_bytes().to_vec());
        cols.insert("ownerId".into(), self.owner_id.as_bytes().to_vec());
        trx.put_obj(Self::type_(), &self.id, cols);
    }

    pub fn pull(mut self, trx: &dyn ITrx) -> File {
        let m = trx.get_obj(Self::type_(), &self.id);
        if !m.is_empty() {
            if let Some(v) = m.get("storeId") {
                self.store_id = String::from_utf8_lossy(v).into_owned();
            }
            if let Some(v) = m.get("ownerId") {
                self.owner_id = String::from_utf8_lossy(v).into_owned();
            }
        }
        self
    }
}
