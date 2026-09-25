//! Serialization and lazy-cache helpers used by the translated engine.

/// Serde adapter matching Go's JSON encoding of `[]byte`.
pub mod bytes_base64 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        Option::<String>::deserialize(deserializer)?.map_or_else(
            || Ok(Vec::new()),
            |text| STANDARD.decode(text).map_err(serde::de::Error::custom),
        )
    }
}

/// Serde adapter matching Go's JSON encoding of `[][]byte`.
pub mod bytes_base64_vec {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::ser::SerializeSeq;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(values: &[Vec<u8>], serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(values.len()))?;
        for value in values {
            sequence.serialize_element(&STANDARD.encode(value))?;
        }
        sequence.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Vec<u8>>, D::Error> {
        Option::<Vec<String>>::deserialize(deserializer)?.map_or_else(
            || Ok(Vec::new()),
            |values| {
                values
                    .iter()
                    .map(|value| STANDARD.decode(value).map_err(serde::de::Error::custom))
                    .collect()
            },
        )
    }
}

/// Clone a lazy cache without forcing initialization.
pub fn clone_once_lock<T: Clone>(source: &std::sync::OnceLock<T>) -> std::sync::OnceLock<T> {
    let cloned = std::sync::OnceLock::new();
    if let Some(value) = source.get() {
        let _ = cloned.set(value.clone());
    }
    cloned
}
