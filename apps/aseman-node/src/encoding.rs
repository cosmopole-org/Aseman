//! Serde encoding helpers for the translated node's wire values.
//!
//! `bytes_base64_vec` (de)serializes `Vec<Vec<u8>>` as a JSON array of base64
//! strings, matching Go's `encoding/json` behaviour for `[][]byte` fields.

/// Serde helpers that (de)serialize `Vec<Vec<u8>>` as a JSON array of base64
/// strings, matching Go's `encoding/json` behaviour for `[][]byte` fields.
pub mod bytes_base64_vec {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::ser::SerializeSeq;
    use serde::{Deserialize, Deserializer, Serializer};

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
            Some(strs) => strs
                .iter()
                .map(|s| STANDARD.decode(s).map_err(serde::de::Error::custom))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ManyBlobs {
        #[serde(with = "bytes_base64_vec", default)]
        blobs: Vec<Vec<u8>>,
    }

    #[test]
    fn bytes_base64_vec_round_trips() {
        let v = ManyBlobs {
            blobs: vec![vec![], vec![0xde, 0xad], vec![0xbe, 0xef]],
        };
        let s = serde_json::to_string(&v).unwrap();
        let parsed: ManyBlobs = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.blobs, v.blobs);
    }

    #[test]
    fn bytes_base64_vec_accepts_null() {
        let parsed: ManyBlobs = serde_json::from_str(r#"{"blobs":null}"#).unwrap();
        assert!(parsed.blobs.is_empty());
    }
}
