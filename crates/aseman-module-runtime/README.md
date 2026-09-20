# aseman-module-runtime

Purpose: verify and cache signed module artifacts, negotiate contracts, supervise
lifecycle/routing generations, validate bootstrap recovery snapshots, and compute
cluster placement reconciliation.

Dependencies: stable module values from `aseman-contracts` plus cryptographic,
serialization, and filesystem support. Invariants: unverified bytes are never cached or
started; callers cannot widen declared permissions; activation is generation-fenced;
and the bootstrap snapshot is a signed, expiring recovery cache rather than business
authority. The public entry point is `src/lib.rs`.

Verify with `cargo test -p aseman-module-runtime` and `cargo xtask fast`.
