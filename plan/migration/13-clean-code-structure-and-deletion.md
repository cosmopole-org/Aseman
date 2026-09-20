# Clean Code, Repository Structure, and Legacy Deletion

## Verdict

The target architecture can make Aseman clean and well decoupled, but the migration is not complete merely when replacement modules work. It is complete only when superseded code paths, duplicate abstractions, obsolete assets, compatibility scaffolding, and undocumented exceptions are removed or deliberately isolated with an expiry.

This document turns cleanliness into verifiable exit criteria rather than subjective review language.

## Current cleanup signals

The following are triage indicators, not standalone defect counts; matches include tests and legitimate low-level code:

- `node/src/main.rs` globally allows dead code, unused imports, module inception, and type complexity.
- The repository contains 16 explicit `dead_code` allowances.
- Textual scans find approximately 1,819 `unwrap`/`expect` calls and 35 `unsafe` references across Rust sources; each production occurrence requires classification.
- The node contains approximately 39 thread sleeps and 54 direct thread spawns, including tests, background polling, and runtime logic.
- Approximately 283 internal `serde_json::Value`/`JsonValue` references indicate broad untyped boundaries.
- Configuration is read in approximately 70 locations across 22 node files.
- The TCP and WebSocket implementations expose parallel session methods such as connection handling, inbound processing, response/update writing, user listeners, and shutdown. Common behavior must move to one transport-neutral session layer.
- The largest source file is approximately 238 KB; several files combine policy, storage, orchestration, protocol, and runtime behavior.
- The repository has multiple Cargo roots and lockfiles instead of one coherent workspace graph.
- `dist/` contains approximately 509 MB of checked-in binaries/runtime artifacts; build outputs and release artifacts obscure the source repository and enlarge history.
- Periodic billing scans the complete `VmBilling::*` key space rather than using an indexed due-work queue.

These observations create a cleanup backlog. They do not authorize mechanical deletion; characterization tests and caller/dependency analysis come first.

## Canonical final hierarchy

```text
Cargo.toml
Cargo.lock
rust-toolchain.toml
README.md
AGENTS.md
ARCHITECTURE.md
CONTRIBUTING.md
SECURITY.md
CHANGELOG.md

apps/                       first-party executable composition roots
  aseman-node/
  asemanctl/
  aseman-vmm/
  aseman-vmm-agent/
  aseman-meter/

crates/                     reusable in-process Rust libraries
  aseman-domain/
  aseman-ports/
  aseman-application/
  aseman-contracts/
  aseman-capsule/
  aseman-config/
  aseman-observability/
  aseman-module-runtime/
  aseman-guest-sdk/

modules/                    independently deployable provider packages
  storage/postgres/
  storage/rocksdb-legacy/
  network/http/
  network/legacy/
  federation/http/
  realtime/durable/
  security/capabilities/
  finance/ledger/
  consensus/hashgraph/
  vmm-backend/nomad/
  vmm-backend/native-legacy/
  runtime/docker/
  runtime/firecracker/
  runtime/wasm/
  runtime/javascript/

contracts/                  source OpenAPI/protobuf/schema definitions
deploy/                     deployment assets and profiles
docs/                       current architecture, concepts, runbooks, ADRs
examples/                   executable examples
tests/                      contract, integration, end-to-end, chaos
evals/agent/                repository-comprehension evaluations
xtask/                      deterministic repository automation
```

There are no generic top-level `models`, `tools`, `utils`, `common`, or `core` dumping grounds. A small local utility module is permitted only when its name states the actual concern, such as `clock`, `encoding`, or `retry`.

## Dependency direction

```text
domain <- ports <- application <- composition roots
   ^         ^          ^                |
   |         |          +--- in-process adapters
   |         +-------------- provider clients
   +------------------------ typed domain values only

external modules <---- versioned wire contracts ----> module runtime/clients
```

Rules:

1. Domain imports no application, provider, transport, database, CLI, or orchestration code.
2. Ports describe behavior needed by application use cases and use typed domain values.
3. Wire contracts are not domain entities; conversion occurs at boundaries.
4. Application imports ports but no concrete implementation.
5. Only executable composition roots choose implementations.
6. External provider modules communicate through versioned contracts and cannot import node internals.
7. Crate and module dependency cycles fail CI.
8. A dependency exception needs an ADR, owner, reason, and removal/review date.

## Removal ledger

Phase 0 creates `docs/migration/removal-ledger.md`. Every current subsystem, directory, protocol, database layout, environment key, binary, and compatibility feature receives one disposition:

```text
KEEP       remains authoritative
MOVE       relocates without semantic change
REWRITE    replaced after parity
MERGE      duplicate behavior consolidated into a named owner
DEPRECATE  retained temporarily with warning and removal release
DELETE     proven unused/obsolete and removed
ARCHIVE    historical documentation only; excluded from builds
GENERATE   replaced by source-generated output
```

Each non-KEEP entry records current owner, target owner, callers, characterization tests, migration, rollback, removal condition, target release, and verification command.

No legacy adapter is permanent by default. Expired compatibility items fail the release gate unless an ADR explicitly extends them.

## Dead-code and unused-dependency policy

1. Remove crate-wide `allow(dead_code)`, `allow(unused_imports)`, `allow(type_complexity)`, and similar suppressions.
2. Fix the cause or place the narrowest possible item-level allowance with a reason and tracking reference.
3. Run compiler warnings, Clippy with warnings denied, unused-dependency analysis, and target/feature matrix checks.
4. Test every supported feature combination; delete feature branches that are neither shipped nor tested.
5. Use coverage and caller analysis as evidence, not as the sole proof that code is dead.
6. Remove stale TODOs, comments, environment keys, routes, schemas, tests, fixtures, and documentation with the implementation they describe.
7. Keep generated source out of handwritten directories and verify regeneration is deterministic.
8. Publish binaries and large runtime libraries as signed releases/OCI artifacts, not source-tree files.

## Duplicate-code policy

Consolidate behavior at the correct abstraction, not through a generic helper that couples unrelated domains.

Priority consolidations:

- TCP, WebSocket, HTTP, and custom transports share authentication, session state, rate limiting, command dispatch, subscription, response, and shutdown logic; adapters own only framing and protocol-specific concerns.
- Client and federation gateways share signed-envelope primitives but retain separate policy and trust contexts.
- All provider lifecycle behavior uses the module supervisor rather than custom per-provider installers.
- All database representations pass through capsule schemas/mappers; physical mappings remain provider-specific.
- Guest database and provider-role selection occurs once from the authenticated workload-to-creature binding; caller input and pooled session residue never select tenancy.
- Runtime lifecycle semantics live in the VMM contract/state machine, while runtime drivers implement only runtime-specific operations.
- Configuration loading occurs once through `AsemanConfig`; direct environment reads outside the config adapter are prohibited.
- Error codes, permissions, routes, capsule kinds, lifecycle states, and provider capabilities have one defining registry.
- Repeated documentation inventories are generated from those registries.

Use duplicate detection as a ratcheted signal. A similarity hit requires review, but identical syntax is not automatically merged when ownership or semantics differ.

## Code-size and responsibility policy

- A source file or function crossing the configured review threshold triggers responsibility analysis and either a split or a documented rationale.
- A module exposes a small intentional API; implementation details remain private.
- Public types live with their owning domain, not in a global model bucket.
- Prefer explicit use-case modules such as `create_workload`, `settle_usage`, and `grant_capability` over large entity action files.
- Tests follow the ownership boundary: unit tests near code, contract tests under contracts/providers, and cross-service scenarios under `tests/`.
- Boilerplate created by schemas/contracts is generated and isolated.

Initial review triggers may be 500 lines per handwritten file, 60 lines per function, and a calibrated complexity threshold. These are prompts for design review, not blind correctness rules; ratchet them downward as legacy files are split.

## Error, type, and API standards

- Internal business flows use typed commands, results, identifiers, states, revisions, money, capabilities, and errors.
- `serde_json::Value`, strings-as-enums, and stringly typed maps are restricted to extension/wire boundaries and immediately validated.
- Libraries use domain-specific `thiserror` enums; binaries add context at composition boundaries.
- Request/runtime paths do not use `unwrap` or `expect`; tests and proven startup invariants may use them with clear intent.
- Every `unsafe` block has a `SAFETY` explanation, a narrow wrapper, and dedicated tests; remove unsafe code that a standard safe abstraction can replace.
- Public traits document authorization, idempotency, consistency, cancellation, timeout, concurrency, and error behavior.
- Backward compatibility is explicit in versioned DTO conversion modules, not scattered conditional logic.

## Concurrency and lifecycle standards

- Use structured async tasks with cancellation tokens, bounded channels, and joined shutdown.
- Do not hold synchronous locks across `.await` or external calls.
- Blocking database/runtime work uses an explicit blocking pool or out-of-process module.
- Production polling loops use cancellable timers, jitter/backoff, health reporting, and bounded work; unmanaged `thread::spawn` plus `sleep` loops are removed.
- Define lock ordering where multiple locks remain.
- Global mutable service singletons are replaced by owned/injected state. Immutable caches may use `OnceLock` with documented semantics.
- Queues define capacity, overflow behavior, retry limits, poison/dead-letter behavior, and observability.
- Shutdown drains or checkpoints in-flight operations within a defined deadline.

## Algorithm and data-structure standards

Correct modularity does not excuse inefficient algorithms. Every hot or unbounded path records its complexity, index assumptions, memory bounds, and failure/backpressure behavior.

Required changes include:

- Billing/metering uses indexed due intervals and batch usage collection, not a full scan of every workload each tick.
- Capsule queries require provider-declared indexes for unbounded datasets; guest-data queries run only after trusted creature database/role resolution and use bounded, declared indexes inside that isolated database.
- Desired/observed VMM reconciliation uses workload IDs, generations, and provider cursors rather than repeated global scans.
- Federation discovery uses indexed signed descriptors and bounded caches rather than scanning peer lists for each request.
- Outbox and realtime replay use ordered `(stream, sequence)` indexes and bounded batches.
- Capability decisions may cache only with policy/key epochs and revocation-safe invalidation.
- Pagination is cursor-based for unbounded collections.
- Retry algorithms use exponential backoff, jitter, deadlines, budgets, and idempotency.
- Hashing, signatures, serialization, and comparisons use canonical encodings and constant-time primitives where secrets are involved.
- Nomad remains the scheduler; Aseman does not duplicate placement algorithms inside the node.

Benchmarks cover scheduling/reconciliation, scoped guest queries, federation lookup/routing, event delivery, and settlement. CI tracks allocations, latency percentiles, throughput, and memory at defined workload sizes; material regressions need an ADR or explicit approval.

## Phase cleanup rule

Every migration phase has two sub-gates:

1. Replacement gate: new path passes parity, contract, failure, and rollback tests.
2. Deletion gate: old callers, code, dependencies, configuration, documentation, artifacts, and tests are removed or placed in an expiring compatibility package.

A phase cannot be marked complete while both implementations remain authoritative.

Examples:

- PostgreSQL cutover is incomplete until node code no longer imports RocksDB/QuestDB and legacy layouts live only in the migration provider/tool.
- VMM extraction is incomplete until the node no longer links runtime libraries or accesses runtime globals.
- HTTP networking is incomplete until shared session logic is removed from legacy TCP/WS implementations.
- Nomad adoption is incomplete if Aseman retains a competing scheduler or ambiguous OpenRaft worker ownership.
- Aseman naming is incomplete until Caspar identifiers remain only in the declared compatibility shim/archive.

## Final cleanup acceptance

- One root Cargo workspace, lockfile, toolchain, lint policy, and task runner describe all first-party Rust code intended to build together.
- No broad lint suppression hides dead, unused, complex, or cyclic code.
- Unused dependencies and unsupported feature branches are absent.
- No concrete provider dependency crosses into domain/application crates.
- No duplicate authoritative implementation exists for storage, scheduling, policy, lifecycle, finance, configuration, or session handling.
- Legacy compatibility is isolated, versioned, warned, tested, and assigned a removal release.
- No production request path uses unchecked panic operations.
- Unsafe code is minimal, documented, and tested.
- Background work is cancellable, bounded, observable, and joined at shutdown.
- Hot paths avoid unindexed full scans and have regression benchmarks.
- Generated binaries, build directories, vendored runtime blobs, and benchmark outputs are not committed as source.
- Architecture, duplicate, dead-code, dependency, docs-drift, and performance checks run in CI with ratcheted baselines.
- The removal ledger has no overdue entry and no item with two authoritative owners.
