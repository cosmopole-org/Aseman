//! The Nomad VMM backend (P6-01, A601).
//!
//! Aseman's desired workloads become Nomad jobs; Nomad's allocations become Aseman
//! observations. The node never sees Nomad: it talks A501 to the VMM service, which
//! talks A504 to this backend.
//!
//! Aseman does not bundle, mirror, or download Nomad (ADR 0002). The operator runs a
//! cluster and points this backend at it.
#![forbid(unsafe_code)]

pub mod backend;
pub mod client;
pub mod job;
pub mod workers;
