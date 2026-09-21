//! Translation of `shell/api/model` — the storage-backed entity models the
//! Caspar shell uses. Each model carries a `type()` discriminator, a `push`
//! that writes its columns/indices through the `ITrx`, and a `pull` /
//! `all` / `list` family that reads them back.

pub mod access;
pub mod chain;
pub mod core_storage;
pub mod creature;
pub mod creature_ports;
pub mod entity;
pub mod file;
pub mod gateway_ports;
pub mod machine_program;
pub mod program_ports;
pub mod session;
pub mod store;
pub mod store_ports;

pub use access::{access_link_key, read_permissions, StorePermissions};
pub use chain::{Chain, ChainShard};
pub use creature::Creature;
pub use entity::Entity;
pub use file::File;
pub use machine_program::Program;
pub use session::Session;
pub use store::Store;
