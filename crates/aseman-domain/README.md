# aseman-domain

Purpose: pure identifiers, value objects, state machines, capability semantics, money,
usage, and domain events.

Dependencies: serialization, error derivation, and UUID value support only. Invariant:
this crate owns no filesystem, network, database, process, environment, runtime, or
framework behavior. Its public entry point is `src/lib.rs`.

Verify with `cargo test -p aseman-domain` and `cargo xtask arch`.
