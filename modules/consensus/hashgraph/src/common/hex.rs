//! Translation of `chain/common/hex.go`.

/// Returns the UPPERCASE hex string representation of `hex_bytes` with the `0X`
/// prefix.
pub fn encode_to_string(hex_bytes: &[u8]) -> String {
    format!("0X{}", ::hex::encode_upper(hex_bytes))
}

/// Converts a hex string with the `0X` prefix to a byte slice.
pub fn decode_from_string(hex_string: &str) -> anyhow::Result<Vec<u8>> {
    Ok(::hex::decode(&hex_string[2..])?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_uses_uppercase_with_0x_prefix() {
        // 0xDEADBEEF — should encode upper-case with the literal "0X" prefix.
        assert_eq!(encode_to_string(&[0xde, 0xad, 0xbe, 0xef]), "0XDEADBEEF");
    }

    #[test]
    fn encode_empty_returns_prefix_only() {
        assert_eq!(encode_to_string(&[]), "0X");
    }

    #[test]
    fn decode_round_trips_arbitrary_bytes() {
        let bytes: Vec<u8> = (0u8..=255).collect();
        let encoded = encode_to_string(&bytes);
        let decoded = decode_from_string(&encoded).expect("decode");
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn decode_accepts_lowercase_payload() {
        // The encode side emits upper-case, but the underlying hex decoder is
        // case-insensitive; verify that property is preserved.
        let decoded = decode_from_string("0Xdeadbeef").expect("decode");
        assert_eq!(decoded, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn decode_rejects_non_hex_input() {
        assert!(decode_from_string("0XZZ").is_err());
    }

    #[test]
    fn decode_returns_empty_for_prefix_only() {
        let v = decode_from_string("0X").expect("decode empty");
        assert!(v.is_empty());
    }
}
