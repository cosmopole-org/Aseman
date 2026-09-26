//! Incoming transport and API adapters — the legacy Caspar shell plus the
//! migrated public gateway (RL-004, RL-009).
//!
//! This is the translation of `kasper/src/shell`: the HTTP/TCP shell surface.
//! It is an adapter layer, not business logic. Each handler family under
//! [`actions`] is being migrated family-by-family into translation-only
//! adapters that resolve an A402 action and delegate to `aseman-application`
//! use cases (RL-004); the `#[cfg(test)]`/`cfg(test)` transport framing is the
//! only thing the legacy TCP/WS paths may keep (RL-009).
//!
//! - [`actions`] — the action handlers (the A402 secured surface).
//! - [`model`] — persisted model adapters (legacy ITrx-backed port adapters).
//! - [`packets`] — request/response/federation-broadcast packet DTOs.
//! - [`authority`] — the legacy A402/A404 decision point for shell surfaces.
//! - [`audit`] — audit record sinks (legacy + PostgreSQL).
//! - [`kasper`] — the composed legacy app (`new_configured_app`).
//! - [`public_http`] — the RL-004 public A701 gateway listener.
//! - [`storage_http`] — the public file-storage HTTP server.
//! - [`workloads`] — the guest-API/VMM ingress wiring.
//! - [`utils`] — small shared helpers (crypto, future, origin, timer).

pub mod actions;
pub(crate) mod audit;
pub(crate) mod authority;
pub mod kasper;
pub mod main_api;
pub mod model;
pub mod packets;
pub(crate) mod public_http;
pub mod storage_http;
pub mod utils;
pub(crate) mod workloads;
