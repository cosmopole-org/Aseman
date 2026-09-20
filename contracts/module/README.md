---
status: CURRENT
owner: module-platform
source_of_truth: contracts/module and crates/aseman-module-runtime
verification: cargo test -p aseman-module-runtime
---

# Module contracts

These are the Phase 2 A201–A208 contracts. `module.schema.json` validates the parsed
`module.toml` model. `control/` and `provider/` freeze the protobuf control/data
conventions mandated by ADR 0003. Artifact trust and execution permissions follow ADR
0015. Trust, lifecycle, placement, and bootstrap schemas are fail-closed inputs to the
supervisor.

Unknown manifest fields are rejected. A `.amod` envelope has bounded, traversal-safe
payload entries; its deterministic payload digest and exact manifest are signed
together. Module bytes become executable candidates only after digest and Ed25519
publisher-signature verification. Activation changes a
monotonic routing generation; the previous ready process remains available during the
rollback window. Bootstrap data is signed and expiring, and is never business-state
authority. Staging also requires a trusted launcher receipt matching every signed
permission and requested secret reference; the supervisor never receives secret values.
