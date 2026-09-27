//! ADR-0004 compatibility name for the canonical Aseman node.

fn main() {
    eprintln!(
        "warning: caspar-node is deprecated; use aseman-node (the alias expires under ADR 0004)"
    );
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("vmm-handoff") {
        std::process::exit(aseman_node::vmm_handoff(&arguments[1..]));
    }
    aseman_node::run();
}
