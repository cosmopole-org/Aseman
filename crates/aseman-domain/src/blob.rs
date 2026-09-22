//! File-byte values (ADR 0027): records carry a blob's evidence, never its bytes.

use serde::{Deserialize, Serialize};

/// What a record keeps about the bytes it points at.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlobEvidence {
    /// The blob's key: a relative, provider-neutral path.
    pub store_key: String,
    /// SHA-256 of the bytes.
    pub content_digest: [u8; 32],
    pub size_bytes: u64,
    pub media_type: String,
}

/// A blob key is a relative path of non-empty segments without `.` or `..`, so no
/// key can name a location outside its store.
#[must_use]
pub fn valid_blob_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('/')
        && !key.contains(['\\', '\0'])
        && key
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_keys_stay_inside_their_store() {
        assert!(valid_blob_key(
            "machines/10@global/entities/main/module.wasm"
        ));
        assert!(valid_blob_key("public-files/abc.type"));
        for key in ["", "/etc/passwd", "a/../b", "./a", "a//b", "a\\b", "a/b/"] {
            assert!(!valid_blob_key(key), "{key}");
        }
    }
}
