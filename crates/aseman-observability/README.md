# aseman-observability

Purpose: shared, redaction-safe trace, metric, and log context. Its public entry point is
`src/lib.rs`.

Invariant: exporters and logging frameworks are adapters and never leak into domain or
application crates; contexts contain identifiers, not credentials or secret values.

Verify with `cargo test -p aseman-observability` and `cargo xtask fast`.
