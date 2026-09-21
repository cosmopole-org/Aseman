# Migration Phases and Gates

The phases are ordered to avoid a rewrite cutover. Each phase produces shippable boundaries and has a mandatory exit gate.

## Phase 0: Baseline, specifications, and ADRs

Work:

- Freeze new cross-layer coupling.
- Inventory APIs, environment variables, database key families, protocols, VM operations, and financial flows.
- Add characterization tests around externally visible behavior.
- Define capsule, VMM, module, federation, security, and finance contracts.
- Write threat, data-flow, trust-boundary, failure, and consistency models.
- Decide Nomad licensing acceptance and Caspar compatibility window.
- Establish benchmark and reliability baselines.
- Label documentation as current, target, legacy, decision, or draft; create the canonical glossary.
- Inventory stale links, routes, configuration keys, schemas, commands, and duplicated feature lists.
- Establish cold-start agent comprehension baselines.
- Create the removal ledger and classify every subsystem, protocol, schema, environment key, artifact, and compatibility path as keep/move/rewrite/merge/deprecate/delete/archive/generate.
- Capture dead-code, duplication, dependency, unsafe, panic, polling, size/complexity, and performance baselines for ratcheting.
- Create and populate the required-artifact register in `16-required-artifacts-and-specification-backlog.md`; generate every Phase 0 artifact before opening dependent work.

Gate: all existing supported operations have a named owner, a contract target, and a characterization test or recorded intentional removal; every Phase 0 artifact is accepted/verified; every blocking ADR listed in `11-decisions-and-risks.md` is accepted before its dependent phase begins.

## Phase 1: Workspace and code boundaries

Work:

- Create the root Cargo workspace and target crate structure.
- Move typed IDs, entities, state machines, money, usage, capabilities, and events into `aseman-domain`.
- Move use cases into `aseman-application`.
- Introduce narrow ports and constructor injection.
- Split large action/driver files by use case and responsibility.
- Replace internal untyped JSON with typed commands/results.
- Introduce structured errors, cancellation, deadlines, and tracing.
- Add deprecated Aseman/Caspar naming aliases.
- Add the pinned toolchain, root `AGENTS.md`, scoped instructions where necessary, and deterministic `cargo xtask` fast/full checks.
- Require crate/module purpose, dependency, invariant, entry-point, and verification documentation.
- Remove broad lint suppressions as responsibilities move; enforce dependency direction and one typed configuration entry point.
- Consolidate transport-neutral session behavior rather than reproducing it in every network adapter.

Gate: domain/application crates compile without drivers; dependency checks, format, Clippy, and unit tests pass; moved behavior has one owner and obsolete callers/dependencies are removed.

## Phase 2: Module runtime and contract testing

Work:

- Implement signed module manifests and contract negotiation.
- Implement supervisor, health, configuration schema, stage/activate/drain/rollback.
- Add module permissions and secret injection.
- Build shared conformance-test harnesses.
- Add CLI module lifecycle commands.
- Generate the module contract/capability catalog and provider-development playbook.

Gate: a sample provider can be installed, validated, switched, drained, and rolled back without rebuilding the node.

## Phase 3: Universal capsules and PostgreSQL

Work:

- Implement capsule envelope, schemas, queries, revisions, relationships, tombstones, and storage routing.
- Catalog every legacy key and map it to a logical capsule kind.
- Implement typed domain repositories on the capsule protocol.
- Create native PostgreSQL tables per core entity type.
- Implement provider-native guest database/namespace provisioning, dedicated creature roles, constrained multi-table/collection schema management, bounded pool isolation, and the trusted creature binding catalog.
- Create native telemetry, audit, finance, outbox, and realtime tables.
- Wrap existing RocksDB/QuestDB behavior in legacy providers.
- Implement canonical capsule export/import, dual write, semantic comparison, and rollback.
- Migrate core state only after consistency verification.
- Generate capsule-kind, logical-schema, physical-mapping, query, and migration inventories.

Gate: all persistent classes use capsules, except VMM-owned observed runtime state, which ADR 0022 keeps in the wrapped legacy storage provider for the embedded/native-legacy VMM until RL-013; PostgreSQL is the default and authoritative provider for every port family except creature balances and the finance ledger, which ADR 0017 keeps legacy-authoritative until P8 (routing per ADR 0026); each creature's guest records and schemas live in its isolated database/namespace rather than a shared guest table; legacy and PostgreSQL providers pass conformance, role/catalog isolation, pool-contamination, schema, and migration/restart tests; node/application crates no longer import RocksDB or QuestDB types.

## Phase 4: Identity, authority, and guest gateway

Work:

- Implement user/node/service/workload identities and rotation.
- Implement capability grants, policy decisions, expiry, revocation, and explanations.
- Enforce authorization for every operation.
- Implement attenuated child-workload delegation.
- Move host calls to the authenticated guest API.
- Implement signed workload challenge/request authentication and enforce trusted workload -> program -> creature -> database/role resolution.
- Add deny-by-default workload network and secret policy.

Gate: property/adversarial tests prove no authority amplification; no cross-creature database connection, catalog access, data access, enumeration, cursor/cache reuse, or pooled-role leakage; and no workload identity, signature, database, namespace, or role spoofing.

## Phase 5: Extract the native VMM

Work:

- Move integrated VMM behavior into the provider-neutral `aseman-vmm` facade and `modules/vmm-backend/native-legacy`.
- Implement the HTTP/OpenAPI contract and generated client.
- Replace every node-to-runtime call with VMM client operations.
- Remove VMM access to node globals, raw storage, shell actions, and finance.
- Implement lifecycle, logs, terminal, events, usage, operation state, and reconciliation.
- Adopt legacy observed VM instances from the Phase 3 VMM handoff inventory into `core.workload` through reconciliation (desired/observed generations per A503), or stop them explicitly; legacy instance records are never treated as desired state (ADR 0022).

Gate: the node binary has no runtime-engine dependency or competing in-process VMM path; obsolete runtime globals/code/configuration are deleted; the native provider passes parity plus VMM conformance tests; every handoff-inventory instance is adopted or explicitly stopped.

## Phase 6: Nomad provider and worker topology

Work:

- Implement desired-state mapping to Nomad jobs/allocations.
- Implement compact and three/five-server cluster profiles.
- Implement worker enroll, cordon, drain, and removal.
- Implement replicated Aseman control-plane deployment, fenced singleton leases, stable endpoint/identity, and replica failover.
- Add Docker/QEMU mappings and hardened runner tasks.
- Implement Firecracker and runtime-specific pause/resume through the worker agent.
- Integrate workload identity, logs, exec, events, and actual allocation statistics.
- Add lost-worker and control-plane recovery.
- Adopt or explicitly release legacy Modal handles, especially `ModalVolume` user data (ADR 0022).

Gate: native and Nomad providers pass the same contract suite; adding/removing a worker or losing a control replica does not change Aseman federation identity; fenced singleton work does not execute twice; no second scheduler or ambiguous OpenRaft worker-management role remains.

## Phase 7: HTTP networking, federation, and realtime

Work:

- Implement the default HTTP client API and streaming endpoints.
- Convert legacy TCP/WS into translation adapters.
- Implement signed node/workload descriptors and discovery.
- Implement home-node routing, signed envelopes, destination authorization, deduplication, retries, and circuit breakers.
- Add durable realtime provider and capsule outbox/checkpoints.
- Audit every federation message type for authentication and replay protection.
- Generate OpenAPI, route, error, authentication, and compatibility catalogs from source definitions.

Gate: two independently administered Aseman clusters perform permitted discovery, lifecycle, terminal, guest API, and realtime operations across federation; forbidden operations fail at the destination; legacy transports contain framing only and share one session/application path.

## Phase 8: Finance extraction and measured billing

Work:

- Separate meter, pricing, ledger, consensus, and enforcement ports.
- Move current Hashgraph behavior behind `ConsensusProvider`.
- Implement actual VMM usage collection and minute buckets.
- Implement deterministic pricing and idempotent double-entry settlement.
- Add backfill, late-sample, insufficient-funds, and reconciliation flows.

Gate: crash/retry/outage testing produces neither duplicate nor missing settled intervals; every charge traces to a raw usage sample and price version.

## Phase 9: CLI, packaging, bootstrap, and operations

Work:

- Complete administration command groups.
- Build separate minimal images and deployment profiles.
- Implement idempotent bootstrap, upgrade, backup, restore, doctor, and support bundle.
- Add observability dashboards and operational runbooks.
- Remove multi-process container assumptions and unsafe worker privileges.
- Publish the authoritative documentation portal, generated CLI/configuration references, and CI-executed examples.
- Move binaries/runtime blobs out of source control and publish them as signed release/OCI artifacts.

Gate: a clean host reaches healthy compact mode through one workflow; a production topology can add/drain workers and restore from backup.

## Phase 10: Hardening and staged rollout

Work:

- Complete unit, conformance, integration, end-to-end, fuzz, property, load, failover, and chaos coverage.
- Add API compatibility, supply-chain, SBOM, signing, and provenance gates.
- Run shadow traffic and canary nodes.
- Run documentation drift, architectural dependency, and repository-comprehension evaluations as release gates.
- Rehearse provider rollback and disaster recovery.
- Deprecate and then remove legacy Caspar names, transports, storage paths, OpenRaft responsibilities, and embedded runtimes only after parity windows close.
- Enforce the two-part replacement/deletion gate for every migrated capability and fail releases with overdue removal-ledger entries.

Gate: every criterion in [10-verification-and-acceptance.md](10-verification-and-acceptance.md) passes and rollback drills are signed off.

Every phase applies [the cleanup rules](13-clean-code-structure-and-deletion.md): a replacement gate proves the new path, then a deletion gate removes the superseded path.

Every phase also closes its section of the [required-artifact register](16-required-artifacts-and-specification-backlog.md). Work packages and agent execution procedure are defined in [15-agent-execution-guide.md](15-agent-execution-guide.md).
