//! The node's telemetry: `/telemetry/health` (always `{"status":"ok"}`) and
//! `/telemetry/snapshot` (the cached snapshot when under 2 s old, else a fresh
//! one), served by a small synchronous HTTP server, plus runtime profiling
//! ([`pprof`]) and resource usage ([`resources`]).

pub mod pprof;
pub mod resources;
pub mod server;

pub use server::start;
