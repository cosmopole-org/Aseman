//! Translation of `src/telemetry/server.go`.
//!
//! Minimal HTTP telemetry server. Two routes:
//!
//!   `/telemetry/health`   — always returns `{"status":"ok"}`.
//!   `/telemetry/snapshot` — returns the cached snapshot if it's < 2 s old,
//!                          otherwise re-collects it from the live node.
//!
//! Storage moved from Badger to RocksDB (single key, `latest_snapshot`,
//! holding the JSON-encoded `Snapshot`). The HTTP layer is a tiny hand-
//! written sync implementation over `std::net::TcpListener` so we avoid
//! pulling in axum/hyper.

pub mod pprof;
pub mod resources;
pub mod server;

pub use server::{start, Snapshot, TelemetryServer};
