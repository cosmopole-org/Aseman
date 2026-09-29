//! Canonical Aseman node executable composition root (RL-001).
//!
//! The implementation lives in this crate. `runner` is the canonical node
//! process: it parses the typed configuration and starts the composition in
//! [`aseman_node::app::NodeApp`], with two maintenance subcommands as special first
//! arguments: the ADR-0022 `vmm-handoff` and the ADR-0036 `storage migrate` (which
//! `asemanctl storage migrate` runs inside a containerized deployment).

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("vmm-handoff") => std::process::exit(aseman_node::vmm_handoff(&arguments[1..])),
        Some("storage") => std::process::exit(aseman_node::storage(&arguments[1..])),
        _ => aseman_node::run(),
    }
}
