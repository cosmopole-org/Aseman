//! Shared low-level helpers used across the translated Caspar node.

/// Serde helpers that (de)serialize `Vec<u8>` as a standard base64 string,
/// matching Go's `encoding/json` behaviour for `[]byte` fields.
pub mod bytes_base64 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let opt = Option::<String>::deserialize(d)?;
        match opt {
            None => Ok(Vec::new()),
            Some(s) => STANDARD
                .decode(s.as_bytes())
                .map_err(serde::de::Error::custom),
        }
    }
}

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

/// Clones a `OnceLock`, preserving an already-initialised value. Used to make
/// translated structs that carry lazy caches cloneable.
pub fn clone_once_lock<T: Clone>(o: &std::sync::OnceLock<T>) -> std::sync::OnceLock<T> {
    let new = std::sync::OnceLock::new();
    if let Some(v) = o.get() {
        let _ = new.set(v.clone());
    }
    new
}

/// Convenience alias mirroring Go's `error` value type.
pub type GoError = anyhow::Error;

/// Opaque value, the translation of Go's empty interface `interface{}` / `any`
/// when it is used for dynamic, downcastable values rather than JSON payloads.
pub type AnyVal = std::sync::Arc<dyn std::any::Any + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct OneBlob {
        #[serde(with = "bytes_base64", default)]
        bytes: Vec<u8>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ManyBlobs {
        #[serde(with = "bytes_base64_vec", default)]
        blobs: Vec<Vec<u8>>,
    }

    #[test]
    fn bytes_base64_serializes_to_standard_base64() {
        let v = OneBlob {
            bytes: vec![0u8, 1, 2, 3, 255],
        };
        let s = serde_json::to_string(&v).unwrap();
        // AAECA/8= is the std-base64 encoding of [0,1,2,3,255].
        assert_eq!(s, r#"{"bytes":"AAECA/8="}"#);
    }

    #[test]
    fn bytes_base64_round_trips_empty_and_nonempty() {
        for payload in [vec![], vec![1, 2, 3], (0u8..=255).collect::<Vec<u8>>()] {
            let v = OneBlob {
                bytes: payload.clone(),
            };
            let s = serde_json::to_string(&v).unwrap();
            let parsed: OneBlob = serde_json::from_str(&s).unwrap();
            assert_eq!(parsed.bytes, payload);
        }
    }

    #[test]
    fn bytes_base64_accepts_json_null_as_empty() {
        let parsed: OneBlob = serde_json::from_str(r#"{"bytes":null}"#).unwrap();
        assert!(parsed.bytes.is_empty());
    }

    #[test]
    fn bytes_base64_rejects_invalid_base64() {
        let err = serde_json::from_str::<OneBlob>(r#"{"bytes":"!!not base64!!"}"#)
            .expect_err("invalid base64 should error");
        let msg = format!("{}", err);
        assert!(msg.to_lowercase().contains("invalid") || msg.to_lowercase().contains("symbol"));
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

    #[test]
    fn clone_once_lock_preserves_initialised_value() {
        let lock: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        lock.set("hello".to_string()).unwrap();
        let cloned = clone_once_lock(&lock);
        assert_eq!(cloned.get(), Some(&"hello".to_string()));
    }

    #[test]
    fn clone_once_lock_starts_uninitialised_when_empty() {
        let lock: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        let cloned = clone_once_lock(&lock);
        assert!(cloned.get().is_none());
        cloned.set(7).unwrap();
        assert_eq!(cloned.get(), Some(&7));
    }
}
