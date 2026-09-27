//! ADR-0004 compatibility name for `aseman-keygen`.

fn main() {
    eprintln!(
        "warning: caspar-keygen is deprecated; use aseman-keygen (the alias expires under ADR 0004)"
    );
    if let Err(error) = aseman_keygen::generate() {
        eprintln!("keygen failed: {error}");
        std::process::exit(1);
    }
}
