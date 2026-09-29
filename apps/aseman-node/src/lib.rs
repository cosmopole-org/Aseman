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
// There is no crate-wide lint suppression. Translated-but-unwired items of the A008
// characterized legacy surface carry a scoped `expect(dead_code, reason = ...)` naming
// the removal-ledger row (RL-002..RL-013) that owns them. `expect` fails the build
// once an item becomes used or is deleted, so each attribute retires with its row.

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

/// `aseman-node storage migrate ...`: the ADR 0036 storage migration (run while the
/// node is stopped). Returns the exit status.
pub fn storage(arguments: &[String]) -> i32 {
    if arguments.first().map(String::as_str) != Some("migrate") {
        eprintln!("{}", aseman_storage_providers::migrate::USAGE);
        return 2;
    }
    let config = match AsemanConfig::from_process_with_dotenv(".env") {
        Ok(config) => config,
        Err(error) => {
            eprintln!("invalid Aseman configuration: {error}");
            return 2;
        }
    };
    match aseman_storage_providers::migrate::command(&config, &arguments[1..]) {
        Ok(report) => {
            print!("{report}");
            0
        }
        Err(error) => {
            eprintln!("storage migrate: {error}");
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
