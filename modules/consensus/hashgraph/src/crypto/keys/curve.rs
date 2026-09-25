//! Translation of `chain/crypto/keys/curve.go`.
//!
//! The Go code carried a runtime `elliptic.Curve` value (`btcec.S256()`). In
//! Rust the curve is the compile-time type `k256::Secp256k1`, so only the
//! curve order constants need translating.

use num_bigint::BigUint;

/// The order N of the secp256k1 curve.
pub fn secp256k1_n() -> BigUint {
    BigUint::parse_bytes(
        b"fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141",
        16,
    )
    .expect("valid secp256k1 N")
}

/// Half of the secp256k1 order, used for signature S-value normalisation.
pub fn secp256k1_half_n() -> BigUint {
    secp256k1_n() / BigUint::from(2u32)
}
