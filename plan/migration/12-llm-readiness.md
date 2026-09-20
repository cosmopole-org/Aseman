# LLM and Agent Readiness

## Objective

An unfamiliar coding agent must be able to determine, with minimal context and no guesswork:

1. What Aseman is today versus what is only planned.
2. Which component owns a behavior or datum.
3. Which dependencies and directions are permitted.
4. How to build, check, test, run, and diagnose the relevant component.
5. Which contracts and invariants a change must preserve.
6. Where configuration, routes, schemas, migrations, and generated artifacts come from.
7. How to make a common change and verify it safely.

LLM friendliness is not a separate documentation dump. It follows from explicit boundaries, typed contracts, deterministic tooling, small modules, executable examples, and mechanically checked documentation.

## Audit findings

- No repository or directory-scoped `AGENTS.md` exists.
- There is no root Cargo workspace, pinned `rust-toolchain.toml`, `CONTRIBUTING.md`, `ARCHITECTURE.md`, `CHANGELOG.md`, or uniform task runner.
- The root README references absent `reports/final` and `node.old` directories.
- Runtime inventories disagree: the root README includes Modal among seven runtimes, while key wiki descriptions and the architecture table list six.
- The current wiki describes the monolith but does not distinguish durable current truth, legacy behavior, and target migration architecture.
- Build, run, VM, cluster, and installation commands are distributed across the root README, wiki, Makefile, large scripts, and CLI help.
- The node reads environment/configuration in approximately 70 places across 22 source files rather than through one discoverable typed configuration model.
- Important source files are extremely large; the largest is approximately 238 KB. Ownership, invariants, and safe change surfaces are hard to infer.
- Action registration is code-driven and lacks a generated, machine-readable route catalog.
- The node does not publish source-of-truth OpenAPI, capsule-schema, module-contract, configuration-schema, or compatibility indexes.
- CI builds and republishes binaries but does not enforce documentation links/examples, architecture rules, format, lint, or contract drift.
- Heavy native dependencies make an undifferentiated full test command slow, while no documented fast change-specific verification path exists.

## Documentation authority model

Documentation must explicitly separate:

```text
CURRENT   behavior available in the checked-out revision
TARGET    approved behavior not yet implemented
LEGACY    compatibility behavior scheduled for removal
DECISION  accepted ADR and its consequences
DRAFT     proposal with no implementation authority
```

Every architectural document has short front matter:

```yaml
status: CURRENT | TARGET | LEGACY | DECISION | DRAFT
owner: component or team
source_of_truth: code/schema/ADR path
last_verified_commit: git revision
verification: command that checks it
```

Generated files state that they are generated and name their generator. Facts have one authoritative home and other documents link to them rather than copying lists that can drift.

## Repository entry points

The target root contains:

```text
README.md                 product summary and five-minute verified path
AGENTS.md                 concise repository-wide agent instructions
ARCHITECTURE.md           short map linking to architecture documentation
CONTRIBUTING.md           human and agent development workflow
SECURITY.md               reporting and security-sensitive change rules
CHANGELOG.md              release-visible behavior changes
rust-toolchain.toml       pinned reproducible Rust toolchain
Cargo.toml                virtual workspace and shared lint/profile policy
docs/                     authoritative documentation portal
contracts/                source wire/schema definitions
examples/                 executable end-to-end examples
evals/agent/              repository-comprehension evaluations
xtask/                    deterministic repository automation
```

`README.md` remains short and truthful. It must not present stale benchmark numbers or unavailable artifacts. It links to generated status/capability pages for revision-specific facts.

## Layered agent instructions

Add a concise root `AGENTS.md` containing only repository-wide rules:

- Product vocabulary and current/target distinction.
- Architecture dependency direction.
- Commands for fast and complete validation.
- Generated-file policy.
- Security and migration invariants.
- Required documentation/ADR updates.
- Paths to task playbooks.

Add nested `AGENTS.md` files only where local rules materially differ, for example:

```text
crates/aseman-domain/AGENTS.md
crates/aseman-contracts/AGENTS.md
apps/aseman-vmm/AGENTS.md
modules/storage/AGENTS.md
modules/vmm-backend/nomad/AGENTS.md
```

Local instruction files state ownership, allowed dependencies, verification commands, and non-obvious invariants. They do not repeat root guidance. Keep the combined instruction chain small and place detailed explanations in linked docs. OpenAI's Codex guidance establishes that `AGENTS.md` files are discovered root-to-working-directory and that closer files take precedence.

## Documentation information architecture

```text
docs/
  README.md
  glossary.md
  status.md
  concepts/
    creature.md
    program-workload.md
    capsule.md
    authority.md
    federation.md
  architecture/
    system-context.md
    containers.md
    dependency-rules.md
    state-ownership.md
    consistency.md
    failure-model.md
    components/
  contracts/
    README.md
    compatibility-policy.md
    error-model.md
  development/
    setup.md
    commands.md
    testing.md
    debugging.md
    common-changes/
  operations/
    compact.md
    cluster.md
    upgrades.md
    backup-restore.md
    runbooks/
  decisions/
    README.md
    NNNN-title.md
  generated/
    workspace.md
    routes.md
    configuration.md
    capsule-kinds.md
    module-capabilities.md
  legacy/caspar/
```

The glossary defines one canonical term for node, physical worker, workload/VM, creature, program, capsule, provider, adapter, module, federation, and consensus. It records deprecated Caspar terminology and replacements.

## Code structure for comprehension

- Prefer names that encode responsibility; remove vague containers such as `models`, `tools`, and monolithic `core` where more precise names exist.
- One module owns one concept or use case. A file above an agreed review threshold requires a split or an explicit rationale.
- Every crate and significant module begins with `//!` documentation containing purpose, ownership, permitted dependencies, invariants, failure semantics, and important entry points.
- Every port documents preconditions, postconditions, authorization, idempotency, consistency, timeout/cancellation, and error semantics for implementers.
- State machines use enums and explicit transitions rather than distributed status strings.
- Identifiers, money, revisions, scopes, operations, and protocol versions use typed newtypes.
- Routes, capsule kinds, permissions, provider capabilities, and configuration keys are declared once in structured registries.
- Generated indexes expose those registries to humans and agents.
- Implementation code links to the relevant ADR/contract; docs link back to defining types and tests.

## Deterministic agent tooling

Provide stable `cargo xtask` commands rather than requiring an agent to reconstruct shell sequences:

```text
cargo xtask doctor
cargo xtask check-fast --changed
cargo xtask check --workspace
cargo xtask test --crate <name>
cargo xtask test-contract <contract>
cargo xtask test-e2e <scenario>
cargo xtask docs-check
cargo xtask architecture-check
cargo xtask inventory --format json
cargo xtask repo-map --format json
```

`doctor` reports missing tools and exact remediation. Commands have bounded scopes, stable output, meaningful exit codes, and a machine-readable JSON mode. Full native builds are not the only way to validate a domain/documentation change.

Pin the Rust toolchain and external tools. Use the root workspace for unified `cargo metadata`, lint policy, dependency discovery, and package-targeted commands.

## Machine-readable knowledge

Generate and validate:

- `contracts/openapi/*.json` for HTTP APIs.
- Protobuf/descriptor sets where RPC uses protobuf.
- Capsule JSON Schemas plus mapping/capability metadata.
- Module manifest JSON Schema.
- Node configuration JSON Schema and environment-variable map.
- Action/route inventory with authentication, capability, request, response, and handler ownership.
- Provider and runtime capability inventory.
- Error-code catalog.
- Workspace/package/dependency map derived from `cargo metadata --format-version`.
- Database logical-schema and migration inventory.

Expose `asemanctl api describe`, `asemanctl capsule schema`, and `asemanctl module contract` for installed-system introspection.

An optional generated `llms.txt` provides a small generic documentation index; it is not a second source of truth. `AGENTS.md` remains the instruction surface for Codex-style repository agents.

## Executable documentation

- Rust API examples are doctests where practical.
- Shell examples run in CI against a disposable compact deployment.
- OpenAPI examples validate against schemas and handlers.
- Configuration examples validate against the configuration schema.
- Capsule examples validate against their registered definitions.
- Links, headings, relative paths, and generated-doc freshness are checked in CI.
- Rustdoc enables broken intra-doc-link and missing crate-level documentation lints.
- CLI reference pages are generated from command definitions rather than handwritten.

## Common-change playbooks

Short playbooks must cover:

- Add an application action/API route.
- Add or evolve a capsule kind.
- Add a core entity and native storage mapping.
- Add or evolve a creature-owned guest table/collection without weakening database/role isolation or capsule export.
- Add a storage/network/security/realtime/finance provider.
- Add a VMM runtime.
- Change a wire contract without breaking compatibility.
- Add configuration or a secret.
- Add an authorization action.
- Add a migration and rollback.
- Diagnose a failed workload or settlement interval.

Each playbook names files to touch, forbidden dependencies, generated outputs, tests, compatibility rules, and rollback obligations.

## LLM comprehension evaluations

Add small, versioned evaluations under `evals/agent/`. Run them in documentation/architecture CI and before major reorganizations.

Cold-start tasks should test whether an unfamiliar agent can:

- Identify the owner and call path of an API action.
- Add a capsule field and all required mappings/migrations.
- Locate the authorization decision for a VM operation.
- Explain desired versus observed workload state.
- Find the smallest correct test command for a changed crate.
- Add a provider without importing it into the domain.
- Determine whether a feature is current, target, or legacy.

Measure correctness, files/tool calls needed, invalid assumptions, unnecessary context read, validation selected, and whether protected invariants were preserved. The goal is not to tune documentation to one model; it is to expose ambiguity and architectural coupling.

## Migration integration

- Phase 0 establishes status labels, glossary, stale-document audit, route/config inventory, and agent comprehension baselines.
- Phase 1 establishes the root workspace/toolchain, root and scoped agent instructions, crate/module docs, and `xtask` fast checks.
- Each contract/module/storage/VMM/network phase generates its machine-readable inventory and common-change playbook with the implementation.
- Phase 9 builds the documentation portal, generated CLI/operations references, and verified examples.
- Phase 10 makes docs drift, architecture rules, and agent comprehension evaluations release gates.

The concrete cold-start workflow, source-to-target map, and phase work packages are in [15-agent-execution-guide.md](15-agent-execution-guide.md). Exact specifications that must be generated rather than guessed are tracked in [16-required-artifacts-and-specification-backlog.md](16-required-artifacts-and-specification-backlog.md).

## Acceptance

- A fresh agent can identify current versus planned behavior from the root in one navigation step.
- Every crate/service/provider has an owner, purpose, dependency rule, entry point, and targeted verification command.
- All public routes, capsule kinds, module contracts, configuration keys, capabilities, and errors are queryable in machine-readable form.
- No README references absent authoritative artifacts.
- Duplicated runtime/feature inventories are generated or mechanically checked.
- Documentation examples and links pass CI.
- A changed public contract fails CI unless its compatibility record and generated docs change.
- Agent evaluations complete common tasks without crossing forbidden dependency boundaries or guessing commands.
