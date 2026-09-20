# aseman-sample-provider

Purpose: a harmless, stateless reference module implementing the generated Phase 2
control and sample contracts. It negotiates protocol v1, reports health/readiness, and
echoes bounded test values; it requests no network egress, mounts, or secrets.

Dependencies: generated `aseman-contracts` bindings plus Tokio/Tonic transport. It owns
no business or persistent state. The executable entry point is `src/main.rs` and the
testable service entry point is `src/lib.rs`.

Verify with `cargo test -p aseman-sample-provider`.
