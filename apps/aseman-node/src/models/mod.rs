//! Shared domain models and driver-port traits.
//!
//! This crate follows a hexagonal architecture: the inner core depends
//! only on the abstractions defined here, never on the concrete drivers
//! that live under [`crate::drivers`] or the shell adapters under
//! [`crate::shell`].
//!
//! # Layout
//!
//! - Domain traits — the contracts the node's core layer is built on:
//!   [`core::ICore`], [`state::IState`], [`info::IInfo`],
//!   [`input::IInput`], [`globe::IGlobe`].
//! - [`action`] — the action layer (pluggable units of work, secured
//!   actions, the action registry).
//! - [`transaction`] — the storage transaction trait used by every
//!   state-modifying code path.
//! - [`packet`] — wire-level packet and request/response types.
//! - [`chain`] — chain consensus, election and staking data types.
//! - [`worker`] — the work item enqueued for VM execution.
//! - [`update`] — a single key/value mutation produced by a transaction.
//! - [`ports`] — the driver port traits (storage, security, signaler,
//!   file, network, vmm) implemented under [`crate::drivers`].

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
pub mod worker;
