//! Legacy state abstractions and driver-port traits (RL-002/003).
//!
//! The wire DTOs that used to live here (`update`, `worker`, the `packet`
//! family, and the chain wire DTOs) have been migrated to
//! `aseman_contracts::legacy_wire` and are re-exported here as compatibility
//! shims until the legacy transports retire (RL-009).
//!
//! What remains is the legacy state/orchestration layer:
//!
//! - [`ports`] — the legacy driver-port traits (`IStorage`, `ISecurity`,
//!   `ISignaler`, `INetwork`, `IWorkloads`, `IRateLimiter`, …). They carry
//!   framework types, so they stay here (replaced by the real `aseman-ports`
//!   traits) rather than polluting the clean ports crate.
//! - [`core`], [`state`], [`info`], [`input`], [`globe`], [`transaction`] —
//!   the legacy orchestrator and state-transaction abstractions (`ICore`,
//!   `IState`, `ITrx`, …). Their behavior migrates to `aseman-application` use
//!   cases as the strangler proceeds (RL-003).
//! - [`action`] — the legacy pluggable action layer (`IAction`,
//!   `ISecureAction`, `IActor`); its handlers become translation-only adapters
//!   over `aseman-application` use cases (RL-004).
//!
//! No new types should be added here; the bucket shrinks, it does not grow.

pub mod action;
pub mod chain;
pub mod core;
pub mod globe;
pub mod info;
pub mod input;
pub mod packet;
pub mod ports;
pub mod state;
pub mod transaction;
pub mod update;
