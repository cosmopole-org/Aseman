//! Node core orchestration and compatibility surface (RL-002, RL-003).
//!
//! Everything under this module is the not-yet-replaced translation of the Go node.
//! It is organized by concern:
//!
//! - [`actor`] — the `Actor` registry and its action model (base actions, the
//!   secured wrapper + guard, and the state carrier).
//! - [`orchestrator`] — the `Core` orchestrator.
//! - [`globe`] — the `Globe` validator coordinator.
//! - [`utils`] — small shared helpers (`GoError`, `AnyVal`) and compatibility
//!   aliases used across the translation.
//!
//! These paths stay until their replacement and deletion gates pass; new code should
//! depend on `aseman-application` use cases over `aseman-ports`, not on this surface.

pub mod actor;
pub mod globe;
pub mod orchestrator;
pub mod trx;
pub mod utils;

pub use actor::{Info, State};
