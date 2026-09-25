//! Translation of `chain/crypto` — cryptographic functions for Babble.

pub mod hash;
pub mod keys;

pub use hash::{sha256, simple_hash_from_two_hashes};
