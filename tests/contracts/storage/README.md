# Storage conformance kit

This crate owns reusable, provider-independent acceptance checks for deterministic
capsule bytes, revision compare-and-set behavior, integrity rejection, tombstones,
typed bounded queries, and exact capability negotiation.

Storage providers implement `StorageProviderHarness` in their integration tests and
run `StorageConformanceKit::run`. The harness is deliberately test-only: production
ports remain in `aseman-ports`, and no concrete database type crosses an architecture
boundary.

Verify with `cargo test -p aseman-storage-conformance`.
