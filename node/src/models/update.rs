use serde::{Deserialize, Serialize};

/// A single key/value mutation produced by a transaction.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Update {
    #[serde(rename = "type")]
    pub typ: String,
    #[serde(rename = "key")]
    pub key: String,
    #[serde(rename = "val", with = "crate::util::bytes_base64", default)]
    pub val: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_serializes_with_short_field_names_and_base64_val() {
        let u = Update {
            typ: "put".to_string(),
            key: "k1".to_string(),
            val: vec![0u8, 1, 2, 3, 255],
        };
        let s = serde_json::to_string(&u).unwrap();
        // Wire keys are `type`, `key`, `val`; val is base64.
        assert_eq!(s, r#"{"type":"put","key":"k1","val":"AAECA/8="}"#);
    }

    #[test]
    fn update_round_trips_through_json() {
        let u = Update {
            typ: "del".to_string(),
            key: "k".to_string(),
            val: vec![],
        };
        let s = serde_json::to_string(&u).unwrap();
        let parsed: Update = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.typ, u.typ);
        assert_eq!(parsed.key, u.key);
        assert_eq!(parsed.val, u.val);
    }

    #[test]
    fn update_val_defaults_to_empty_when_absent() {
        let parsed: Update = serde_json::from_str(r#"{"type":"del","key":"k"}"#).unwrap();
        assert!(parsed.val.is_empty());
    }
}
