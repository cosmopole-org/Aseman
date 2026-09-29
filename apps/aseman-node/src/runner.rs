//! The node executable: it parses the typed configuration and starts the node
//! ([`aseman_node::app::NodeApp`]). Two maintenance subcommands run instead when
//! named first: `vmm-handoff` (ADR 0022) and `storage migrate` (ADR 0036, which
//! `asemanctl storage migrate` runs inside a containerized deployment).

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("vmm-handoff") => std::process::exit(aseman_node::vmm_handoff(&arguments[1..])),
        Some("storage") => std::process::exit(aseman_node::storage(&arguments[1..])),
        _ => aseman_node::run(),
    }
}
