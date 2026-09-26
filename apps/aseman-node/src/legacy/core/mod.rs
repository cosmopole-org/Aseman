//! Domain core — the orchestrator and the building blocks it owns:
//! the actor registry + actor model, the globe validator-set coordinator,
//! the worker pool, and the in-process logger.
//!
//! The core depends only on `crate::models` (ports and shared
//! domain types); drivers (`crate::drivers`) sit behind those ports.

pub mod actor;
pub mod core_orchestrator;
pub mod globe;
pub mod logger;
pub mod pool;

pub use core_orchestrator::Core;
