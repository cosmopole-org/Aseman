//! Translation of `chain/babble` — the engine that wires `Config`, `Store`,
//! `Transport`, `Proxy` and `Node` together.

#[allow(clippy::module_inception)] // Go-translation module name matches its directory
pub mod babble;

pub use babble::{Babble, load_key_for_config};
