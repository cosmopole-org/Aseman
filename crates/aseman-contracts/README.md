# aseman-contracts

Purpose: stable wire DTOs, protocol errors, version negotiation, and compatibility
fixtures, with source schemas under `contracts/`.

Invariant: contracts own wire values and never become the domain model; domain does not
depend on this crate. The public entry point is `src/lib.rs`.

Verify with `cargo test -p aseman-contracts` and `cargo xtask fast`.
