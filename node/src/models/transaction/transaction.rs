use std::collections::HashMap;

use anyhow::Result;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::{Map, Value};

use crate::models::update::Update;

/// Generic model contract — parses a value of type `T` out of a transaction.
pub trait IModel<T> {
    fn type_(&self) -> String;
    fn parse(&self, trx: &dyn ITrx) -> T;
}

/// A storage transaction over the node's key/value database.
///
/// Methods take `&self`; the concrete implementation
/// ([`crate::core::actor::model::trx`]) carries interior mutability so a
/// transaction handle can be cloned and shared freely.
pub trait ITrx: Send + Sync {
    fn del_key(&self, key: &str);
    fn get_by_prefix(&self, prefix: &str) -> Vec<String>;
    fn has_obj(&self, typ: &str, key: &str) -> bool;
    fn get_index(
        &self,
        typ: &str,
        from_column: &str,
        to_column: &str,
        from_column_val: &str,
    ) -> String;
    fn put_index(
        &self,
        typ: &str,
        from_column: &str,
        to_column: &str,
        from_column_val: &str,
        to_column_val: Vec<u8>,
    );
    fn del_index(&self, typ: &str, from_column: &str, to_column: &str, from_column_val: &str);
    fn has_index(
        &self,
        typ: &str,
        from_column: &str,
        to_column: &str,
        from_column_val: &str,
    ) -> bool;
    fn get_column(&self, typ: &str, obj_id: &str, column_name: &str) -> Vec<u8>;
    fn get_links_list(
        &self,
        p: &str,
        offset: i64,
        count: i64,
        should_be_global: &[bool],
    ) -> Result<Vec<String>>;
    fn search_link_vals_list(
        &self,
        typ: &str,
        from_column: &str,
        to_column: &str,
        word: &str,
        filter: &HashMap<String, String>,
        offset: i64,
        count: i64,
    ) -> Result<Vec<String>>;
    #[allow(clippy::too_many_arguments)]
    fn search_link_keys_list_by_prefix(
        &self,
        p: &str,
        typ: &str,
        filter: &HashMap<String, String>,
        in_arr_filter: &HashMap<String, Vec<String>>,
        offset: i64,
        count: i64,
        should_be_global: &[bool],
    ) -> Result<Vec<String>>;
    fn get_obj_list(
        &self,
        typ: &str,
        obj_ids: &[String],
        query: &HashMap<String, String>,
        meta: &[i64],
    ) -> Result<HashMap<String, HashMap<String, Vec<u8>>>>;
    fn get_link(&self, key: &str) -> String;
    fn put_link(&self, key: &str, value: &str);
    fn put_bytes(&self, key: &str, value: Vec<u8>);
    fn get_bytes(&self, key: &str) -> Vec<u8>;
    fn put_string(&self, key: &str, value: &str);
    fn get_string(&self, key: &str) -> String;
    fn get_obj(&self, typ: &str, key: &str) -> HashMap<String, Vec<u8>>;
    fn put_obj(&self, typ: &str, key: &str, keys: HashMap<String, Vec<u8>>);
    fn put_json(&self, key: &str, path: &str, json_obj: &Value, merge: bool) -> Result<()>;
    fn del_json(&self, key: &str, path: &str);
    fn get_json(&self, key: &str, path: &str) -> Result<Map<String, Value>>;
    fn get_pri_key(&self, tag: &str) -> Option<RsaPrivateKey>;
    fn get_pub_key(&self, tag: &str) -> Option<RsaPublicKey>;
    fn updates(&self) -> Vec<Update>;
    fn commit(&self);
    fn discard(&self);
}
