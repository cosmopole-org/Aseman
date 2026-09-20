# Verification and Acceptance

## Architecture

- `aseman-domain` and `aseman-application` do not import concrete adapters.
- `aseman-node` contains no Docker, Firecracker, WasmEdge, Nomad, or runtime-plugin implementation.
- No Rust dynamic library ABI is required for provider installation.
- Every provider publishes a compatible manifest and passes its conformance suite.
- Module activation is health-checked, drainable, auditable, and rollback-safe.
- Every migrated capability has exactly one authoritative implementation and owner.
- Crate/module dependency cycles and forbidden imports fail CI.
- Every requirement in the traceability matrix has implementation evidence, migration/rollback coverage, and a passing acceptance owner.
- No phase crosses its gate with a `MISSING` or `DRAFT` required artifact.

## Capsule storage

- Core, guest, telemetry, audit, finance, outbox, realtime, and module data all use capsule contracts.
- Each core entity type receives a separate SQL table or document collection where the database supports such structures.
- PostgreSQL does not emulate core state through a universal KV or JSON table.
- SQL/document guest data uses a dedicated database or equivalently isolated provider namespace and role per creature; no universal shared guest table/collection is required or authoritative.
- VMs from separate programs of one creature authenticate with their own workload keys and resolve to the same creature database/role.
- Within policy and provider capabilities, a creature can create, alter, index, and remove multiple tables/collections in its database while protected capsule metadata remains intact.
- VMs of different creatures cannot connect to, read, enumerate, modify, infer, catalog-inspect, cache-hit, subscribe to, or reuse cursors for each other's guest databases.
- VMs cannot nominate or override the trusted creature, provider, database, namespace, or role.
- Signed guest-data authentication rejects expired, replayed, revoked, wrong-audience, wrong-key-epoch, and payload-substituted requests.
- Connection cancellation, errors, retries, and pool reuse cannot retain or leak a previous creature's database or assumed role.
- Capsule revisions prevent lost updates.
- Provider migration preserves kind, schema version, ownership, guest table/collection definitions, role bindings, relationships, revisions, tombstones, and integrity hashes.
- RocksDB-to-PostgreSQL migration changes the physical representation instead of retaining legacy key layouts.
- Incompatible consistency or query requirements block provider activation with actionable diagnostics.
- Interrupted migration resumes safely; cutover can roll back during the retention window.
- Audit data remains append-only and verifiable after migration.

## VMM and cluster

- Native and Nomad providers pass identical lifecycle and failure semantics.
- Providers can switch without rebuilding the node.
- Create/start/stop/pause/resume/delete, logs, terminal, exec, events, and usage use the VMM HTTP contract.
- Idempotent retries do not create duplicate workloads.
- Compact mode works on one host.
- Production mode supports three/five Nomad servers and expandable workers.
- Adding/removing a worker does not change the Aseman node identity.
- Losing and replacing an Aseman control-plane replica does not change node identity or duplicate fenced singleton effects.
- Firecracker runs only on eligible, policy-authorized workers.
- `raw_exec` is absent from production defaults.
- Controller/provider restarts reconcile desired and observed state.

## Security

- Every sensitive action reaches the policy decision point.
- A child cannot amplify the parent's rights, duration, scope, network access, secrets, or delegation depth.
- Revocation reaches active sessions and descendant grants within the defined bound.
- Workloads have no direct provider/database credentials or administration connection; all guest-data access uses the authenticated Aseman proxy and a provider-enforced creature role.
- Nomad identity authenticates allocation identity but never bypasses Aseman policy.
- Federation requests, responses, updates, and descriptors are signed, expiry checked, and replay protected.
- Destination nodes independently authorize cross-node actions.
- All policy decisions and administrative mutations produce audit capsules.

## Network, federation, and realtime

- HTTP is the shipped default for clients and federation.
- Custom protocol adapters pass the same application contract tests.
- Every authenticated VM can resolve every registered target VM's minimal descriptor containing home-node ID, address, and public key; bulk enumeration and private metadata remain policy-controlled.
- Discovery alone grants no operational rights.
- Authorized lifecycle, signalling, and terminal operations work across nodes.
- Realtime events survive process restart and are deduplicated on redelivery.
- Federation partitions recover without duplicate execution or routing loops.

## Finance and metering

- Charges use measured, normalized usage rather than only declared allocation.
- Every interval settles at most once and can be backfilled after outage.
- Every ledger mutation is balanced and idempotent.
- Every charge traces to workload, interval, raw sample, pricing version, and consensus result.
- Insufficient-funds actions follow policy and use authorized VMM operations.
- Consensus providers can change only at a verified financial epoch.

## Packaging and operations

- Node, VMM, and meter have separate least-privilege artifacts.
- Compact setup completes through one idempotent command.
- Re-running bootstrap is safe.
- Failed stages resume or roll back without destroying persisted data.
- Backup and clean-environment restore are tested.
- Formatting, Clippy, tests, compatibility, security, SBOM, and signing gates pass in CI.
- Releases do not commit generated binaries into the source repository.

## Documentation and agent comprehension

- Root and appropriately scoped `AGENTS.md` files provide concise, non-duplicated instructions and targeted validation commands.
- Current, target, legacy, decision, and draft behavior is visibly distinguished.
- Every crate/service/provider documents purpose, ownership, dependency rules, invariants, entry points, and checks.
- Public routes, capsule kinds, module contracts, configuration, capabilities, schemas, errors, and workspace topology are machine-readable.
- CLI, API, schema, and configuration reference documents are generated from their defining source.
- Documentation links, snippets, examples, doctests, and generated freshness pass CI.
- Repository documentation references no missing authoritative artifact.
- Agent comprehension evaluations locate correct owners, choose bounded checks, and preserve dependency/security/storage invariants.
- A cold-start agent using only the repository and migration files can identify the active work package, required inputs, current and target owners, commands, security/performance obligations, migration, rollback, removal work, and completion evidence without session context.

## Clean code, algorithms, and repository structure

- One root workspace, lockfile, pinned toolchain, lint policy, and task runner cover first-party Rust code built together.
- The final hierarchy uses explicit `apps`, `crates`, `modules`, `contracts`, `deploy`, `docs`, `examples`, `tests`, and `xtask` ownership.
- Broad dead/unused/type-complexity lint suppressions are absent; narrow exceptions contain reasons and tracking references.
- Unused dependencies, unsupported feature branches, duplicate authoritative implementations, and overdue compatibility paths are absent.
- Direct environment reads outside the typed configuration adapter are rejected.
- Internal business APIs do not use unvalidated JSON values or strings as identifiers/states.
- Runtime request paths do not panic through unchecked `unwrap`/`expect`.
- Unsafe code is minimal, narrowly wrapped, documented with safety invariants, and tested.
- Background work is cancellable, bounded, observable, and joined at shutdown; no unmanaged production sleep/poll loops remain.
- Shared TCP/WS/HTTP session behavior has one transport-neutral owner.
- Billing, metering, discovery, reconciliation, outbox, and guest-data paths use bounded batches, bounded/lazy per-database pools, and declared indexes rather than unbounded full scans.
- Hot paths have benchmarks and ratcheted latency, throughput, memory, and allocation budgets.
- Generated binaries, build outputs, vendored runtime blobs, and transient benchmark results are not committed as source.
- The removal ledger has no overdue item and no entry with two authoritative owners.

## Required test suites

```text
unit                  domain rules and state machines
architecture          forbidden dependency and layer checks
capsule conformance   schemas, queries, transactions, revisions, isolation
VMM conformance       lifecycle, idempotency, logs, events, usage, failures
policy property       delegation attenuation and scope isolation
contract              OpenAPI/wire compatibility
integration           real PostgreSQL, event bus, Nomad, provider processes
end-to-end             compact and multi-node federation workflows
fuzz                   parsers, envelopes, query AST, migration inputs
load                   scheduling, federation, guest data, realtime, metering
chaos                  node/provider/worker/database/network failure
migration              backfill, dual write, cutover, rollback, resume
disaster recovery      signed backup and clean restore
agent comprehension    cold-start ownership, change-path, and invariant tasks
```
