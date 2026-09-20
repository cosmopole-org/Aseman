//! Translation of `chain/crypto/hash.go`.

use sha2::{Digest, Sha256};

/// Returns the SHA256 hash of the data.
#[allow(non_snake_case)]
pub fn sha256(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

/// Returns the SHA256 hash of the concatenation of `left` and `right`.
pub fn simple_hash_from_two_hashes(left: &[u8], right: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_empty_vector() {
        let got = sha256(b"");
        let expected =
            ::hex::decode("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
                .unwrap();
        assert_eq!(got, expected);
    }

    #[test]
    fn sha256_known_abc_vector() {
        // SHA-256("abc") = ba7816...
        let got = sha256(b"abc");
        let expected =
            ::hex::decode("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
                .unwrap();
        assert_eq!(got, expected);
    }

    #[test]
    fn sha256_is_deterministic() {
        let a = sha256(b"hello world");
        let b = sha256(b"hello world");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn sha256_is_sensitive_to_single_bit() {
        let a = sha256(b"hello world");
        let b = sha256(b"hello worle");
        assert_ne!(a, b);
    }

    #[test]
    fn simple_hash_from_two_hashes_matches_concatenation() {
        let left = b"left";
        let right = b"right";
        let combined: Vec<u8> = [&left[..], &right[..]].concat();
        assert_eq!(simple_hash_from_two_hashes(left, right), sha256(&combined));
    }

    #[test]
    fn simple_hash_from_two_hashes_is_not_commutative() {
        let a = b"a";
        let b = b"b";
        assert_ne!(
            simple_hash_from_two_hashes(a, b),
            simple_hash_from_two_hashes(b, a)
        );
    }

    #[test]
    fn simple_hash_from_two_hashes_handles_empty_left() {
        // Folding empty into something must equal sha256(something).
        assert_eq!(simple_hash_from_two_hashes(&[], b"x"), sha256(b"x"));
    }
}
