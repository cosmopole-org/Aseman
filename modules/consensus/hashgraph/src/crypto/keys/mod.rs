//! Translation of `chain/crypto/keys` — the public-key cryptography used
//! throughout Babble.
//!
//! Babble uses ECDSA over the secp256k1 curve. The Go code used `btcsuite`'s
//! secp256k1 implementation; this translation uses the `k256` crate, which
//! implements the same curve.

pub mod curve;
pub mod key_reader_writer;
pub mod private_key;
pub mod public_key;
pub mod signature;

pub use curve::{secp256k1_half_n, secp256k1_n};
pub use key_reader_writer::{KeyReaderWriter, SimpleKeyfile};
pub use private_key::{dump_private_key, generate_ecdsa_key, parse_private_key, private_key_hex};
pub use public_key::{from_public_key, public_key_hex, public_key_id, to_public_key};
pub use signature::{decode_signature, encode_signature, sign, verify};
