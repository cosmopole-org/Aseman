//! Deprecated Caspar executable alias governed by ADR 0004.

fn main() {
    eprintln!(
        "warning: caspar-node is deprecated; use aseman-node (compatibility window: ADR 0004)"
    );
    caspar_node::run();
}
