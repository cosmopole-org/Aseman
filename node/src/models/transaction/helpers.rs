use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};

/// Marshal an arbitrary object then unmarshal it into a JSON object map.
pub fn object_to_map<T: Serialize>(obj: &T) -> Result<Map<String, Value>> {
    let data = serde_json::to_vec(obj)?;
    let m: Map<String, Value> = serde_json::from_slice(&data)?;
    Ok(m)
}

/// Serialize a JSON object map then deserialize it into a concrete object.
pub fn map_to_object<T: DeserializeOwned>(m: &Map<String, Value>) -> Result<T> {
    let data = serde_json::to_vec(m)?;
    let obj: T = serde_json::from_slice(&data)?;
    Ok(obj)
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
