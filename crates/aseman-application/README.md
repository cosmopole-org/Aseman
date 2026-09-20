# aseman-application

Purpose: transport-neutral use cases, authorization orchestration, sagas, and
reconciliation.

Dependencies: `aseman-domain`, `aseman-ports`, and error derivation only. Invariant:
application code imports no drivers and receives all effects through narrow ports. The
public entry point is `src/lib.rs`.

Verify with `cargo test -p aseman-application` and `cargo xtask arch`.
