//! The signed-packet transports' wire values: the packets the TCP, WebSocket,
//! federation, and chain transports frame, and the chain's ordered requests.

pub mod chain;
pub mod packet;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Serde helper that (de)serializes `Vec<u8>` as a standard base64 string,
/// matching Go's `encoding/json` behaviour for `[]byte` fields.
pub mod bytes_base64 {
    use super::*;

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let opt = Option::<String>::deserialize(d)?;
        match opt {
            None => Ok(Vec::new()),
            Some(value) => STANDARD
                .decode(value.as_bytes())
                .map_err(serde::de::Error::custom),
        }
    }
}

/// Serde helper that (de)serializes `Vec<Vec<u8>>` as a JSON array of base64
/// strings, matching Go's `encoding/json` behaviour for `[][]byte` fields.
pub mod bytes_base64_vec {
    use super::*;
    use serde::ser::SerializeSeq;

    pub fn serialize<S: Serializer>(v: &[Vec<u8>], s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(v.len()))?;
        for item in v {
            seq.serialize_element(&STANDARD.encode(item))?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Vec<u8>>, D::Error> {
        let opt = Option::<Vec<String>>::deserialize(d)?;
        match opt {
            None => Ok(Vec::new()),
            Some(strings) => strings
                .iter()
                .map(|value| STANDARD.decode(value).map_err(serde::de::Error::custom))
                .collect(),
        }
    }
}

/// A single key/value mutation produced by a transaction.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Update {
    #[serde(rename = "type")]
    pub typ: String,
    #[serde(rename = "key")]
    pub key: String,
    #[serde(rename = "val", with = "crate::wire::bytes_base64", default)]
    pub val: Vec<u8>,
}

/// A transaction queued for execution by a virtual machine worker.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Trx {
    #[serde(rename = "key")]
    pub key: String,
    #[serde(rename = "payload")]
    pub payload: String,
    #[serde(rename = "signature")]
    pub signature: String,
    #[serde(rename = "userId")]
    pub user_id: String,
    #[serde(rename = "machineId")]
    pub machine_id: String,
    #[serde(rename = "runtime")]
    pub runtime: String,
    #[serde(rename = "gasLimit")]
    pub gas_limit: i64,
    #[serde(rename = "callbackId")]
    pub callback_id: String,
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

    #[test]
    fn worker_trx_round_trips_with_legacy_field_names() {
        let t = Trx {
            key: "wk".to_string(),
            payload: "{}".to_string(),
            signature: "sig".to_string(),
            user_id: "u".to_string(),
            machine_id: "m".to_string(),
            runtime: "wasm".to_string(),
            gas_limit: 10,
            callback_id: "cb".to_string(),
        };
        let s = serde_json::to_string(&t).unwrap();
        let parsed: Trx = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.user_id, "u");
        assert_eq!(parsed.gas_limit, 10);
    }
}
