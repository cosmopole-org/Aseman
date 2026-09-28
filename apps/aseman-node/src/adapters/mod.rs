//! Driver adapters — concrete implementations of the port traits defined
//! in [`crate::models::ports`].
//!
//! - `blob_store` — storage-root [`aseman_ports::BlobStore`] provider (ADR 0027)
//! - `ratelimit` — cross-protocol token-bucket request admission control
//! - `signaler` — in-process pub/sub event bus
//! - `storage` — the storage module: loads the provider plugin (ADR 0036)
//! - `security` — RSA/ECDSA signing and verification
//! - `vmm` — in-process virtual-machine driver (wasm / docker /
//!   javascript / elpify / elpian / firecracker)
//! - `network` — chain consensus + TCP/WS client + federation transports
//! - `cluster` — OpenRaft-replicated geo-distributed instance mesh

pub(crate) mod blob_store;
pub mod gateway_subs;
pub mod module_admin;
pub mod network;
pub mod ratelimit;
pub mod security;
pub mod signaler;
pub mod storage;
pub mod vmm;
