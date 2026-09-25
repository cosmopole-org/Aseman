//! Translation of `shell/api/model/chain.go`.

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::models::transaction::ITrx;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Chain {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "storeId", default)]
    pub store_id: String,
}

impl Chain {
    pub fn type_() -> &'static str {
        "Chain"
    }

    pub fn push(&self, trx: &dyn ITrx) {
        let mut cols: HashMap<String, Vec<u8>> = HashMap::new();
        cols.insert("id".into(), self.id.as_bytes().to_vec());
        cols.insert("storeId".into(), self.store_id.as_bytes().to_vec());
        trx.put_obj(Self::type_(), &self.id, cols);
    }

    pub fn delete(&self, trx: &dyn ITrx) {
        for c in ["|", "id", "storeId"] {
            trx.del_key(&format!("obj::{}::{}::{}", Self::type_(), self.id, c));
        }
    }

    pub fn pull(mut self, trx: &dyn ITrx) -> Chain {
        let m = trx.get_obj(Self::type_(), &self.id);
        if !m.is_empty() {
            if let Some(v) = m.get("id") {
                self.id = String::from_utf8_lossy(v).into_owned();
            }
            if let Some(v) = m.get("storeId") {
                self.store_id = String::from_utf8_lossy(v).into_owned();
            }
        }
        self
    }

    pub fn all(
        trx: &dyn ITrx,
        offset: i64,
        count: i64,
        query: &HashMap<String, String>,
    ) -> Result<Vec<Chain>> {
        let objs = trx.get_obj_list("Chain", &["*".to_string()], query, &[offset, count])?;
        let mut entities: Vec<Chain> = objs
            .into_iter()
            .filter_map(|(id, m)| {
                if m.is_empty() {
                    return None;
                }
                let mut c = Chain {
                    id,
                    ..Default::default()
                };
                if let Some(v) = m.get("storeId") {
                    c.store_id = String::from_utf8_lossy(v).into_owned();
                }
                Some(c)
            })
            .collect();
        entities.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entities)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChainShard {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "workChainId", default)]
    pub work_chain_id: String,
}

impl ChainShard {
    pub fn type_() -> &'static str {
        "ChainShard"
    }

    pub fn push(&self, trx: &dyn ITrx) {
        let mut cols: HashMap<String, Vec<u8>> = HashMap::new();
        cols.insert("id".into(), self.id.as_bytes().to_vec());
        cols.insert("workChainId".into(), self.work_chain_id.as_bytes().to_vec());
        trx.put_obj(Self::type_(), &self.id, cols);
    }

    pub fn pull(mut self, trx: &dyn ITrx) -> ChainShard {
        let m = trx.get_obj(Self::type_(), &self.id);
        if !m.is_empty() {
            if let Some(v) = m.get("id") {
                self.id = String::from_utf8_lossy(v).into_owned();
            }
            if let Some(v) = m.get("workChainId") {
                self.work_chain_id = String::from_utf8_lossy(v).into_owned();
            }
        }
        self
    }

    pub fn all(
        trx: &dyn ITrx,
        offset: i64,
        count: i64,
        query: &HashMap<String, String>,
    ) -> Result<Vec<ChainShard>> {
        let objs = trx.get_obj_list("ChainShard", &["*".to_string()], query, &[offset, count])?;
        let mut entities: Vec<ChainShard> = objs
            .into_iter()
            .filter_map(|(id, m)| {
                if m.is_empty() {
                    return None;
                }
                let mut c = ChainShard {
                    id,
                    ..Default::default()
                };
                if let Some(v) = m.get("workChainId") {
                    c.work_chain_id = String::from_utf8_lossy(v).into_owned();
                }
                Some(c)
            })
            .collect();
        entities.sort_by(|a, b| match a.work_chain_id.cmp(&b.work_chain_id) {
            std::cmp::Ordering::Equal => a.id.cmp(&b.id),
            other => other,
        });
        Ok(entities)
    }
}
