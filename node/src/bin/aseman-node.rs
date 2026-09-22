//! Canonical Aseman node executable.
//!
//! `aseman-node` runs the node; `aseman-node vmm-handoff ...` is the operator's ADR
//! 0022 legacy VM handoff (see `docs/operations/vmm-handoff-runbook.md`).

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("vmm-handoff") {
        std::process::exit(caspar_node::vmm_handoff(&arguments[1..]));
    }
    caspar_node::run();
}
