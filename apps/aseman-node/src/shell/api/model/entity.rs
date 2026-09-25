//! Translation of `shell/api/model/entity.go`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::models::transaction::ITrx;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entity {
    #[serde(rename = "programId", default)]
    pub program_id: String,
    #[serde(rename = "entityId", default)]
    pub entity_id: String,
    #[serde(rename = "entityType", default)]
    pub entity_type: String,
    #[serde(rename = "imageName", default)]
    pub image_name: String,
}

impl Entity {
    pub fn type_() -> &'static str {
        "Entity"
    }

    pub fn key(&self) -> String {
        format!("{}::{}", self.program_id, self.entity_id)
    }

    pub fn push(&self, trx: &dyn ITrx) {
        let mut cols: HashMap<String, Vec<u8>> = HashMap::new();
        cols.insert("programId".into(), self.program_id.as_bytes().to_vec());
        cols.insert("entityId".into(), self.entity_id.as_bytes().to_vec());
        cols.insert("entityType".into(), self.entity_type.as_bytes().to_vec());
        cols.insert("imageName".into(), self.image_name.as_bytes().to_vec());
        trx.put_obj(Self::type_(), &self.key(), cols);
    }

    pub fn pull(mut self, trx: &dyn ITrx) -> Entity {
        let m = trx.get_obj(Self::type_(), &self.key());
        if !m.is_empty() {
            self.program_id = take_string(&m, "programId");
            self.entity_id = take_string(&m, "entityId");
            self.entity_type = take_string(&m, "entityType");
            self.image_name = take_string(&m, "imageName");
        }
        self
    }
}

fn take_string(m: &HashMap<String, Vec<u8>>, key: &str) -> String {
    m.get(key)
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default()
}
