//! Translation of `shell/api/model/store.go`.

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::models::transaction::ITrx;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub tag: String,
    #[serde(rename = "parentId", default)]
    pub parent_id: String,
    #[serde(rename = "persHist", default)]
    pub pers_hist: bool,
    #[serde(rename = "isPublic", default)]
    pub is_public: bool,
    #[serde(rename = "memberCount", default)]
    pub member_count: i32,
    #[serde(rename = "signalCount", default)]
    pub signal_count: i64,
}

impl Store {
    pub fn type_() -> &'static str {
        "Store"
    }

    pub fn push(&self, trx: &dyn ITrx) {
        let mut me = self.clone();
        if me.member_count < 1 {
            me.member_count = 1;
        }
        let mut cols: HashMap<String, Vec<u8>> = HashMap::new();
        cols.insert("tag".into(), me.tag.as_bytes().to_vec());
        cols.insert("parentId".into(), me.parent_id.as_bytes().to_vec());
        cols.insert(
            "isPublic".into(),
            vec![if me.is_public { 0x01 } else { 0x00 }],
        );
        cols.insert(
            "persHist".into(),
            vec![if me.pers_hist { 0x01 } else { 0x00 }],
        );
        cols.insert(
            "memberCount".into(),
            (me.member_count as u32).to_le_bytes().to_vec(),
        );
        cols.insert(
            "signalCount".into(),
            (me.signal_count as u64).to_le_bytes().to_vec(),
        );
        trx.put_obj(Self::type_(), &me.id, cols);
    }

    pub fn delete(&self, trx: &dyn ITrx) {
        for c in [
            "|",
            "id",
            "tag",
            "parentId",
            "isPublic",
            "persHist",
            "memberCount",
            "signalCount",
        ] {
            trx.del_key(&format!("obj::{}::{}::{}", Self::type_(), self.id, c));
        }
        trx.del_json(&format!("StoreMeta::{}", self.id), "metadata");
    }

    /// A store filled from its object columns.
    pub(crate) fn from_columns(id: String, columns: &HashMap<String, Vec<u8>>) -> Store {
        let mut store = Store {
            id,
            ..Default::default()
        };
        Store::fill(&mut store, columns);
        store
    }

    fn fill(d: &mut Store, m: &HashMap<String, Vec<u8>>) {
        if let Some(v) = m.get("tag") {
            d.tag = String::from_utf8_lossy(v).into_owned();
        }
        if let Some(v) = m.get("parentId") {
            d.parent_id = String::from_utf8_lossy(v).into_owned();
        }
        if let Some(v) = m.get("memberCount") {
            if v.len() == 4 {
                d.member_count = i32::from_le_bytes(v.as_slice().try_into().unwrap());
            }
        }
        if let Some(v) = m.get("signalCount") {
            if v.len() == 8 {
                d.signal_count = i64::from_le_bytes(v.as_slice().try_into().unwrap());
            }
        }
        if let Some(v) = m.get("isPublic") {
            d.is_public = v == &[0x01];
        }
        if let Some(v) = m.get("persHist") {
            d.pers_hist = v == &[0x01];
        }
    }

    pub fn pull(mut self, trx: &dyn ITrx) -> Store {
        let m = trx.get_obj(Self::type_(), &self.id);
        if !m.is_empty() {
            Store::fill(&mut self, &m);
        }
        self
    }

    pub fn list(
        trx: &dyn ITrx,
        prefix: &str,
        global: bool,
        filter: &HashMap<String, String>,
        in_arr_filter: &HashMap<String, Vec<String>>,
        offset: i64,
        count: i64,
    ) -> Result<Vec<Store>> {
        let list = trx.search_link_keys_list_by_prefix(
            prefix,
            "Store",
            filter,
            in_arr_filter,
            offset,
            count,
            &[global],
        )?;
        let objs = trx.get_obj_list("Store", &list, &HashMap::new(), &[])?;
        let mut entities: Vec<Store> = objs
            .into_iter()
            .filter_map(|(id, m)| {
                if m.is_empty() {
                    return None;
                }
                let mut s = Store {
                    id,
                    ..Default::default()
                };
                Store::fill(&mut s, &m);
                Some(s)
            })
            .collect();
        entities.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entities)
    }

    pub fn all(
        trx: &dyn ITrx,
        offset: i64,
        count: i64,
        query: &HashMap<String, String>,
    ) -> Result<Vec<Store>> {
        let objs = trx.get_obj_list("Store", &["*".to_string()], query, &[offset, count])?;
        let mut entities: Vec<Store> = objs
            .into_iter()
            .filter_map(|(id, m)| {
                if m.is_empty() {
                    return None;
                }
                let mut s = Store {
                    id,
                    ..Default::default()
                };
                Store::fill(&mut s, &m);
                Some(s)
            })
            .collect();
        entities.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entities)
    }

    pub fn search(
        trx: &dyn ITrx,
        offset: i64,
        count: i64,
        word: &str,
        filter: &HashMap<String, String>,
    ) -> Result<Vec<Store>> {
        let links =
            trx.search_link_vals_list("Store", "title", "id", word, filter, offset, count)?;
        let objs = trx.get_obj_list("Store", &links, &HashMap::new(), &[])?;
        let mut entities: Vec<Store> = objs
            .into_iter()
            .filter_map(|(id, m)| {
                if m.is_empty() {
                    return None;
                }
                let mut s = Store {
                    id,
                    ..Default::default()
                };
                Store::fill(&mut s, &m);
                Some(s)
            })
            .collect();
        entities.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(entities)
    }
}
