//! Translation of `core/module/core/core.go` — the `Core` compatibility orchestrator.
//!
//! `Core` is the `ICore` implementation, the central object that gives every
//! action / driver access to the rest of the system. It owns the `ITools`
//! bundle (storage, security, signaler, network, vmm), the `IActor`
//! registry, the `IGlobe` validator-set coordinator, and the chain dispatch
//! channel.
//!
//! The chain dispatch goroutine, the election ticker, and the chain-packet
//! callbacks all stay as background threads spawned by `Load`.
//!
//! - [`types`] — the `Core`/`Tools` type definitions.
//! - [`finance`] — the core-owned finance state (free nodes + cost model).
//! - [`constructor`] — construction (`new` / `new_configured`).
//! - [`crypto`] — RSA private-key parsing and PSS-SHA256 signing.
//! - [`chain`] — chain packet handling and chain-op submission.
//! - [`icore`] — the `ICore` trait impl + ADR-0026 state helpers.
//! - [`load`] — `run` and the strongly-typed `load_inner`.

mod chain;
mod constructor;
mod crypto;
mod finance;
pub(crate) mod icore;
mod load;
mod types;

pub use types::Core;
