# Module conformance kit

Purpose: reusable provider-independent checks for manifest safety, cached artifact
presence, protocol negotiation, required capabilities, bounds, and conformance-report
evidence. Provider crates add behavior-specific vectors while keeping these checks.

The public entry point is `src/lib.rs`. Verify with
`cargo test -p aseman-module-conformance`.
