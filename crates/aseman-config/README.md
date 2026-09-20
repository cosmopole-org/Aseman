# aseman-config

Purpose: the single typed entry point for environment/file configuration and legacy-key
canonicalization. The public entry point is `src/lib.rs`.

Invariant: composition roots supply its values; domain and application never read the
environment. Canonical/legacy conflicts fail closed, secret values are not logged, and
legacy aliases follow ADR 0004.

Verify with `cargo test -p aseman-config` and `cargo xtask fast`.
