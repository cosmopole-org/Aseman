//! Unique id strings.

use uuid::Uuid;

/// Returns a pair of UUIDs joined by `-`. Used as request ids, packet ids,
/// pool tails, etc.
pub fn secure_unique_string() -> String {
    format!("{}-{}", Uuid::new_v4(), Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_string_is_unique() {
        let a = secure_unique_string();
        let b = secure_unique_string();
        assert_ne!(a, b);
        assert!(a.contains('-'));
    }

    #[test]
    fn unique_string_has_two_uuid_segments() {
        let s = secure_unique_string();
        // Each UUID contains 4 dashes (8-4-4-4-12). Two UUIDs joined by `-`
        // gives 4 + 1 + 4 = 9 dashes total.
        assert_eq!(s.matches('-').count(), 9);
        // The 5th dash (index 4) is the separator between the two UUIDs.
        let sep = s.match_indices('-').nth(4).expect("separator").0;
        let (a, b) = (&s[..sep], &s[sep + 1..]);
        Uuid::parse_str(a).expect("first half should be a UUID");
        Uuid::parse_str(b).expect("second half should be a UUID");
    }
}