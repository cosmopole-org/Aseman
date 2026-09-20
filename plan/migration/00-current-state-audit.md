# Current-State Audit

## Summary

The repository contains substantial implementations for VM runtimes, networking, federation, storage, security, consensus, billing, a CLI, and deployment. The principal problem is structural: these capabilities are compiled and wired into one node rather than exposed as independently replaceable modules.

The Rust codebase is approximately 87,000 lines, with approximately 55,000 in the node. Several action and driver files are large enough to conceal policy, persistence, transport, and orchestration in the same units.

## Findings

### VMM

- `node/src/drivers/mod.rs` explicitly describes the VMM as in-process.
- `node/src/models/ports/vmm.rs::IVmm` combines lifecycle, runtime discovery, Docker identity, signalling, database operations, locks, host calls, and HTTP forwarding.
- Runtime plugins are statically registered in `node/crates/caspar-vm-plugins/src/lib.rs`.
- `casparctl vms enable/disable/sync` changes the generated compile-time aggregation and therefore does not provide runtime module replacement.
- The existing VMM HTTP listener is guest workload ingress, not an Aseman-to-VMM control API.

### Storage

- `IStorage` exposes concrete RocksDB `TransactionDB` and QuestDB/PostgreSQL-wire pool types.
- Core persistence is based on physical keys and prefixes that are visible outside a database adapter.
- The shipped database topology is RocksDB plus QuestDB, not the required PostgreSQL default.
- Storage, VM transactions, and host actions cross boundaries through the broad core service locator.

### Network, federation, and cluster

- Client traffic primarily uses custom TLS TCP and WebSocket transports.
- Federation uses a custom framed transport and peer lists rather than a complete signed global node/workload directory.
- Some federated requests pass through signature verification, but response and update paths need a unified authenticated-envelope audit.
- OpenRaft is embedded as a cluster mechanism; it is not the requested Nomad server/client worker topology.
- Current discovery does not guarantee that every permitted VM can resolve another VM's home-node ID, address, and public key.

### Security

- Key management, signing, ownership checks, and some resource ACLs already exist.
- There is no single policy decision point covering all operations.
- Current source documentation states that `runVm` is deliberately not ACL-controlled; this conflicts with the required administrator-governed authority model.
- Parent-to-child delegation, attenuation, expiry, and revocation are not modeled generally.
- Transport identity and application capability authority are not consistently separated.

### Realtime

- Realtime signalling is primarily an in-process callback/map implementation.
- It is not durable, replayable, or horizontally shared between node instances.

### Finance and metering

- The code contains wallet, payment-lock, Hashgraph, and recurring billing behavior.
- A recurring thread invokes the VM billing logic every 15 seconds, despite a stale comment saying the scheduler is not started.
- Charges are based on declared resources and fixed per-minute price rather than normalized historical usage from the VMM.
- Financial rules, consensus ordering, VM actions, and persistence are too tightly coupled.

### Packaging and administration

- The Docker image combines concerns that should be separate services.
- `run-nodes.sh` installs many dependencies but is too large and imperative to be the long-term adaptive installer.
- The CLI lacks runtime module install/activate/rollback, provider migration, generalized policy, federation trust, worker lifecycle, and metering reconciliation commands.
- CI builds artifacts but lacks complete format, lint, test, compatibility, security, and artifact-signing gates.

### LLM and agent discoverability

- No root or scoped `AGENTS.md` provides repository instructions, dependency rules, or targeted verification commands.
- No root Cargo workspace or pinned Rust toolchain provides one machine-readable project graph and reproducible entry point.
- `CONTRIBUTING.md`, `ARCHITECTURE.md`, and `CHANGELOG.md` are absent.
- The root README references absent `reports/final` and `node.old` paths.
- Runtime lists disagree between the root README and wiki pages, including whether Modal is present.
- Approximately 70 environment reads are distributed across 22 node source files instead of one typed/config-schema-backed model.
- Large files, vague ownership boundaries, code-driven route registration, and missing machine-readable contracts make safe change scope difficult to infer.
- There is no documented fast validation path that avoids building unrelated native dependencies.
- See [LLM and agent readiness](12-llm-readiness.md) for the target information architecture and evaluation plan.

### Cleanliness, duplication, and lifecycle

- The node crate globally suppresses dead-code, unused-import, module-inception, and type-complexity warnings.
- A triage scan finds 16 explicit dead-code allowances; production and test occurrences must be classified before deletion.
- TCP and WebSocket clients implement parallel connection/session behavior that should be consolidated below transport framing.
- Broad JSON values, status/action strings, service globals, direct thread creation, and sleep-based polling obscure typed ownership and shutdown behavior.
- The repository is split across multiple Cargo roots/lockfiles and contains large committed distribution/runtime artifacts.
- Billing performs a full `VmBilling::*` scan on its recurring pass, which will not scale with large workload counts.
- The existing plan needs explicit deletion gates to prevent the strangler migration from retaining two permanent authoritative paths. See [clean code, structure, and deletion](13-clean-code-structure-and-deletion.md).

## Baseline verification

- `cargo fmt --all -- --check` currently reports formatting differences.
- `cargo test --workspace --all-targets` was started but spent more than eight minutes compiling native RocksDB dependencies and did not reach a test result before being stopped. This is neither a pass nor a failure.
- The audit made no source changes and left the worktree clean.

## Existing behavior to preserve during migration

- Creature, program, entity, store, and resource concepts.
- VM lifecycle operations and runtime selection where semantics are valid.
- Docker, Firecracker, WASM, JavaScript, Elpian, Elpify, and external runtime support.
- VM logs, signalling, HTTP ingress, and terminal-related behavior.
- Existing identity, signing, federation, wallet, and Hashgraph behavior until replacements pass parity gates.
- Existing Caspar names and environment keys as deprecated aliases for a defined compatibility window.
