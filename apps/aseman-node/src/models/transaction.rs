//! JSON object helpers used by the action layer. Transactions are the storage
//! module's [`crate::core::trx::Trx`] (ADR 0036).

use anyhow::Result;
use serde::Serialize;
use serde_json::{Map, Value};

/// Marshal an arbitrary object then unmarshal it into a JSON object map.
pub fn object_to_map<T: Serialize>(obj: &T) -> Result<Map<String, Value>> {
    let data = serde_json::to_vec(obj)?;
    let m: Map<String, Value> = serde_json::from_slice(&data)?;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_to_map_extracts_fields() {
        #[derive(Serialize)]
        struct Sample {
            name: String,
            count: i64,
        }
        let m = object_to_map(&Sample {
            name: "alpha".to_string(),
            count: 5,
        })
        .unwrap();
        assert_eq!(m.get("name").and_then(Value::as_str), Some("alpha"));
        assert_eq!(m.get("count").and_then(Value::as_i64), Some(5));
    }
}
