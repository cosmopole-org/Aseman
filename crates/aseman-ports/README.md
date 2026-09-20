# aseman-ports

Purpose: application-facing behavioral interfaces using only domain values.

Dependencies: `aseman-domain` and error derivation only. Invariant: implementations and
concrete storage, network, scheduler, consensus, and runtime types stay in adapters or
providers. The public entry point is `src/lib.rs`.

Verify with `cargo test -p aseman-ports` and `cargo xtask arch`.
