//! The Aseman node.
//!
//! - [`app`] — the composition root: configuration in, the node started.
//! - `node` — the running node and its components.
//! - `actions` — the operations, one router behind every surface (ADR 0039).
//! - `state` — the node's records and ports over the storage module (ADR 0036).
//! - `transports` — public HTTP, the signed-packet transports, the chain,
//!   federation, public files, and module administration.
//! - `live` — live delivery to connected clients.
//! - `workloads` — the VMM client and the host calls guests make.
//! - `identity`, `ratelimit`, `storage`, `blobs`, `util` — the node's keys,
//!   admission control, storage, files, and small helpers.
//! - `observability` — telemetry, profiling, and resource reporting.

mod actions;
pub mod app;
mod blobs;
mod identity;
mod live;
mod node;
mod observability;
mod ratelimit;
mod state;
mod storage;
mod transports;
mod util;
mod workloads;

use aseman_config::AsemanConfig;

/// `aseman-node vmm-handoff ...`: the operator's ADR 0022 handoff of VM instances
/// a Aseman-era node ran to the configured VMM (run while the node is stopped). Returns the exit
/// status.
pub fn vmm_handoff(arguments: &[String]) -> i32 {
    let config = match AsemanConfig::from_process_with_dotenv(".env") {
        Ok(config) => config,
        Err(error) => {
            eprintln!("invalid Aseman configuration: {error}");
            return 2;
        }
    };
    match crate::workloads::vmm::handoff(&config, arguments) {
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
/// [`app::NodeApp`].
pub fn run() {
    match app::NodeApp::from_process().and_then(app::NodeApp::start) {
        Ok(()) => {}
        Err(error) => eprintln!("{error}"),
    }
}
