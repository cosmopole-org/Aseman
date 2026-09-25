//! Deprecated Caspar CLI alias governed by ADR 0004.

fn main() {
    eprintln!("warning: casparctl is deprecated; use asemanctl (compatibility window: ADR 0004)");
    asemanctl::main();
}
