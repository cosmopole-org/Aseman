//! Canonical Aseman node executable composition root (RL-001).
//!
//! The implementation lives in this crate. Deprecated Caspar binaries depend one-way
//! on it during the ADR 0004 compatibility window.

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("vmm-handoff") {
        std::process::exit(aseman_node::vmm_handoff(&arguments[1..]));
    }
    aseman_node::run();
}
