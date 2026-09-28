//! Storage transaction abstractions.
//!
//! [`ITrx`] is the transaction handle threaded through every state-modifying
//! code path; [`IModel`] is the generic model contract layered on top of it.
//! The JSON map ↔ object conversion utilities used by model implementations
//! are exposed as [`object_to_map`] and [`map_to_object`].

use crate::models::update::Update;
use anyhow::Result;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Marshal an arbitrary object then unmarshal it into a JSON object map.
pub fn object_to_map<T: Serialize>(obj: &T) -> Result<Map<String, Value>> {
    let data = serde_json::to_vec(obj)?;
    let m: Map<String, Value> = serde_json::from_slice(&data)?;
    Ok(m)
}
/// Serialize a JSON object map then deserialize it into a concrete object.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "RL-002: legacy model surface kept until its deletion gate"
    )
)]
pub fn map_to_object<T: DeserializeOwned>(m: &Map<String, Value>) -> Result<T> {
    let data = serde_json::to_vec(m)?;
    let obj: T = serde_json::from_slice(&data)?;
    Ok(obj)
}
/// Generic model contract — parses a value of type `T` out of a transaction.
#[expect(
    dead_code,
    reason = "RL-002: legacy model surface kept until its deletion gate"
)]
pub trait IModel<T> {
    fn type_(&self) -> String;
    fn parse(&self, trx: &dyn ITrx) -> T;
}
/// A storage transaction over the node's key/value database.
///
/// Methods take `&self`; the concrete implementation
/// ([`crate::adapters::rocksdb::trx`]) carries interior mutability so a
/// transaction handle can be cloned and shared freely.
pub trait ITrx: Send + Sync {
    /// Whether this transaction was opened read-only.
    fn readonly(&self) -> bool {
        false
    }
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "RL-002: legacy model surface kept until its deletion gate"
        )
    )]
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
    #[allow(clippy::too_many_arguments)] // legacy storage query surface
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "RL-002: legacy model surface kept until its deletion gate"
        )
    )]
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "RL-002: legacy model surface kept until its deletion gate"
        )
    )]
    fn put_string(&self, key: &str, value: &str);
    fn get_string(&self, key: &str) -> String;
    fn get_obj(&self, typ: &str, key: &str) -> HashMap<String, Vec<u8>>;
    fn put_obj(&self, typ: &str, key: &str, keys: HashMap<String, Vec<u8>>);
    fn put_json(&self, key: &str, path: &str, json_obj: &Value, merge: bool) -> Result<()>;
    fn del_json(&self, key: &str, path: &str);
    fn get_json(&self, key: &str, path: &str) -> Result<Map<String, Value>>;
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "RL-002: legacy model surface kept until its deletion gate"
        )
    )]
    fn updates(&self) -> Vec<Update>;
    /// Write the transaction atomically. A failed write is reported (LD-10).
    fn commit(&self) -> Result<()>;
    fn discard(&self);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Sample {
        name: String,
        count: i64,
        flags: Vec<bool>,
    }
    #[test]
    fn object_to_map_extracts_fields() {
        let s = Sample {
            name: "alpha".to_string(),
            count: 5,
            flags: vec![true, false],
        };
        let m = object_to_map(&s).unwrap();
        assert_eq!(m.get("name").and_then(Value::as_str), Some("alpha"));
        assert_eq!(m.get("count").and_then(Value::as_i64), Some(5));
        assert_eq!(
            m.get("flags").and_then(Value::as_array).map(Vec::len),
            Some(2)
        );
    }
    #[test]
    fn map_to_object_inverse_of_object_to_map() {
        let s = Sample {
            name: "round".to_string(),
            count: -7,
            flags: vec![],
        };
        let m = object_to_map(&s).unwrap();
        let back: Sample = map_to_object(&m).unwrap();
        assert_eq!(back, s);
    }
    #[test]
    fn map_to_object_rejects_type_mismatch() {
        let mut m = serde_json::Map::new();
        // `count` is i64 in Sample, supply a string instead.
        m.insert("name".to_string(), Value::String("x".to_string()));
        m.insert(
            "count".to_string(),
            Value::String("not a number".to_string()),
        );
        m.insert("flags".to_string(), Value::Array(vec![]));
        let err = map_to_object::<Sample>(&m).expect_err("type mismatch");
        let msg = format!("{}", err);
        assert!(msg.contains("count") || msg.contains("invalid"), "{}", msg);
    }
    #[test]
    fn object_to_map_handles_unit_struct_via_serde_json_value() {
        // A Map<String, Value> round-trip through itself should be the identity.
        let mut original = serde_json::Map::new();
        original.insert("a".to_string(), Value::from(1));
        original.insert("b".to_string(), Value::from("two"));
        let m = object_to_map(&original).unwrap();
        assert_eq!(m, original);
    }
}
