//! Deprecated Caspar key-generation alias governed by ADR 0004.

fn main() {
    eprintln!(
        "warning: caspar-keygen is deprecated; use aseman-keygen (compatibility window: ADR 0004)"
    );
    aseman_node::keygen::main();
}
