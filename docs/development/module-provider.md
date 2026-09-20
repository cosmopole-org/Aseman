---
status: CURRENT
owner: module-platform
source_of_truth: contracts/module and docs/decisions/0003-module-rpc-and-versioning.md and docs/decisions/0015-module-artifact-trust-and-execution.md
verification: cargo test -p aseman-module-runtime -p aseman-sample-provider
---

# Develop a module provider

1. Choose one declared module kind and copy the complete manifest shape from
   `contracts/module/fixtures/valid/sample-module.toml`.
2. Add provider-specific protobuf RPCs under `contracts/module/<kind>/v1/`; retain the
   common request metadata and structured errors from the control/provider contracts.
3. Generate bindings through `aseman-contracts`; application code must use a narrow
   port and an adapter rather than generated RPC types directly.
4. Declare the least network, mount, and secret-reference permissions. Secret values do
   not belong in manifests, bootstrap snapshots, logs, or command arguments. A platform
   launcher must enforce the exact declaration, resolve only those secret references,
   and return the receipt required by `ModuleSupervisor::stage`.
5. Implement negotiation, bounded messages, health/readiness, cancellation, deadlines,
   idempotency, drain, and shutdown. Missing capabilities return `UNSUPPORTED`.
6. Run the shared module conformance suite and provider-specific failure tests.
7. Package a digest-pinned `.amod`/OCI artifact with SBOM and license manifest, sign its
   domain-separated digest using an enrolled Ed25519 publisher key, then use the
   install → validate → stage → activate workflow.
8. Keep the previous routing generation alive through the rollback window. Stateful
   providers also follow their capability-specific migration journal.

Run `cargo xtask fast` before review and `cargo xtask full` for composition changes.
