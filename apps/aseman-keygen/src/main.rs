//! Translation of `keygen/keygen.go` — generate an ECDSA keypair and
//! persist it under `~/.babble/{priv_key,key.pub}`.
//!
//! The Babble Go CLI used `keys.GenerateECDSAKey` (a thin wrapper around
//! `crypto/ecdsa.GenerateKey(elliptic.P256())`) and dumped the D-value as
//! plain hex. The Rust port matches that byte format using the `k256`
//! crate directly so this binary stays self-contained.

/// Generate the legacy-compatible Babble key pair for the configured home directory.
fn main() {
    if let Err(e) = aseman_keygen::generate() {
        eprintln!("keygen failed: {}", e);
        std::process::exit(1);
    }
}
