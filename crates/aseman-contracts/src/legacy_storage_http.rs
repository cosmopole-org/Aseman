//! Pure compatibility rules for the legacy public-storage HTTP adapter.

#[must_use]
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[must_use]
pub fn sanitize_content_type(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|character| *character != '\r' && *character != '\n')
        .take(128)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "application/octet-stream".to_owned()
    } else {
        cleaned.to_owned()
    }
}

#[must_use]
pub fn escape_json(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_id_compatibility_rejects_path_syntax() {
        assert!(is_safe_id("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_safe_id("legacy_blob-01"));
        for value in ["", ".", "..", "a/b", r"a\b", "a.type", "a%2fb", "has space"] {
            assert!(!is_safe_id(value), "accepted unsafe ID: {value}");
        }
        assert!(!is_safe_id(&"a".repeat(129)));
    }

    #[test]
    fn content_type_and_json_escaping_match_the_adapter() {
        assert_eq!(sanitize_content_type(""), "application/octet-stream");
        assert_eq!(
            sanitize_content_type("image/png\r\nX-Injected: yes"),
            "image/pngX-Injected: yes"
        );
        assert_eq!(sanitize_content_type(&"x".repeat(140)).len(), 128);
        assert_eq!(escape_json(r#"disk \ "full""#), r#"disk \\ \"full\""#);
    }
}
