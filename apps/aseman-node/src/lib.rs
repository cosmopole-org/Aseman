// Caspar node — Rust translation of the Caspar (kasper) Go node.
//
// This crate is the Aseman node composition root. It is organized by hexagonal
// layer:
//
// - [`api`] — inbound transport and API adapters (the shell surface, RL-004).
// - [`adapters`] — concrete outbound/infrastructure adapters (storage, network,
//   security, signaler, VMM, cluster, ...).
// - [`core`] — node orchestration and compatibility state that survives until
//   the removal-ledger rows pass their gates.
// - [`observability`] — telemetry, profiling, and resource reporting.
// - [`encoding`], [`sync`] — small named helpers.
//
// [`app::NodeApp`] is where the pieces are wired.
//
// The single remaining crate-wide suppression is `#![allow(dead_code)]`. It is
// strangler-gated: the Go translation carries translated-but-not-yet-wired items that
// are the A008 characterized legacy surface, so they must survive until the
// removal-ledger rows (RL-002..RL-012) pass their replacement and deletion gates.
// The lint must not be removed by deleting that surface ahead of the gates; it is
// removed when the last row retires.

#![allow(dead_code)]

mod adapters;
mod api;
pub mod app;
mod core;
pub mod encoding;
mod models;
mod observability;
mod sync;

use aseman_config::AsemanConfig;

/// `aseman-node vmm-handoff ...`: the operator's ADR 0022 handoff of legacy VM
/// instances to the configured VMM (run while the node is stopped). Returns the exit
/// status.
pub fn vmm_handoff(arguments: &[String]) -> i32 {
    let config = match AsemanConfig::from_process_with_dotenv(".env") {
        Ok(config) => config,
        Err(error) => {
            eprintln!("invalid Aseman configuration: {error}");
            return 2;
        }
    };
    match crate::api::workloads::handoff(&config, arguments) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("vmm-handoff: {error}");
            1
        }
    }
}

/// Bring the node up: parse the typed configuration and start the composition in
/// [`app::NodeApp`]. Entry point used by `main.rs` and the `caspar-node` alias.
pub fn run() {
    match app::NodeApp::from_process().and_then(app::NodeApp::start) {
        Ok(()) => {}
        Err(error) => eprintln!("{error}"),
    }
}
