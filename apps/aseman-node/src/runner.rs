//! Canonical Aseman node executable composition root (RL-001).
//!
//! The implementation lives in this crate. `runner` is the canonical node
//! process: it parses the typed configuration and starts the composition in
//! [`aseman_node::app::NodeApp`], with the ADR-0022 `vmm-handoff` maintenance
//! subcommand available as the only special first argument.

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("vmm-handoff") {
        std::process::exit(aseman_node::vmm_handoff(&arguments[1..]));
    }
    aseman_node::run();
}
