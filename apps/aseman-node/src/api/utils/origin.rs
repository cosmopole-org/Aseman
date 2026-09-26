//! Translation of `shell/utils/origin/origin.go`.

/// Returns the part of `id` after the last `@`, or an empty string if `id`
/// doesn't contain one.
pub fn find_origin(id: &str) -> String {
    if id.is_empty() {
        return String::new();
    }
    match id.rsplit_once('@') {
        Some((_, after)) => after.to_string(),
        None => String::new(),
    }
}

/// Maps the synthetic `"global"` origin to an empty string, leaving every
/// other value alone.
pub fn local_only(value: &str) -> String {
    if value == "global" {
        String::new()
    } else {
        value.to_string()
    }
}

/// Convenience: `local_only(find_origin(id))`.
pub fn find_origin_local(id: &str) -> String {
    local_only(&find_origin(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Translation of `origin_test.go` — Go test names preserved as comments.

    #[test]
    fn find_origin_returns_after_at() {
        assert_eq!(find_origin("alice@foo"), "foo");
        assert_eq!(find_origin("a@b@c"), "c");
    }

    #[test]
    fn find_origin_empty_inputs() {
        assert_eq!(find_origin(""), "");
        assert_eq!(find_origin("noseparator"), "");
    }

    #[test]
    fn local_only_filters_global() {
        assert_eq!(local_only("global"), "");
        assert_eq!(local_only("alice"), "alice");
    }
}
