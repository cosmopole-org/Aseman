# Aseman Modular Platform Migration Plan

Status: proposed architecture and execution plan  
Scope: migration of the legacy Caspar codebase into the modular Aseman platform  
Rule: this folder is the source of truth for the refactor until its decisions are promoted to permanent ADRs and product documentation.

The repository and these files contain the complete migration context. The conversation that produced them is not an implementation dependency. New agents start with [the agent execution guide](15-agent-execution-guide.md) and must treat unresolved artifacts/ADRs as blockers rather than guessing.

## Outcome

Aseman becomes a federated, extensible VM-hosting control plane. The node contains domain and application logic but does not depend directly on Nomad, Docker, Firecracker, PostgreSQL, Hashgraph, NATS, or any network transport. It uses versioned contracts implemented by replaceable modules.

The shipped defaults are:

- HTTP for client, federation, and node-to-VMM APIs.
- PostgreSQL for persistence.
- Nomad as the VMM provider, subject to the licensing decision in [11-decisions-and-risks.md](11-decisions-and-risks.md).
- The extracted legacy VMM as a supported compatibility provider.
- A durable event-bus provider for realtime delivery, with an in-memory development implementation.
- The existing Hashgraph behavior behind a replaceable consensus provider.
- Deny-by-default workload access governed by attenuable capabilities.

All persisted data uses the capsule protocol. Core entities map to native, separate tables or collections. Guest data maps to a provider-native database or isolated namespace per creature, protected by a dedicated provider role assumed only by the authenticated Aseman proxy; a creature may define multiple tables or collections inside it.

## Documents

1. [Current-state audit](00-current-state-audit.md)
2. [Target architecture](01-target-architecture.md)
3. [Module system](02-module-system.md)
4. [Adaptive capsule storage](03-adaptive-capsule-storage.md)
5. [VMM, Nomad, and runtimes](04-vmm-nomad-and-runtimes.md)
6. [Security and workload authority](05-security-and-authority.md)
7. [Network, federation, and realtime](06-network-federation-realtime.md)
8. [Finance and resource metering](07-finance-and-metering.md)
9. [CLI, packaging, and operations](08-cli-packaging-operations.md)
10. [Migration phases and gates](09-migration-phases.md)
11. [Verification and acceptance](10-verification-and-acceptance.md)
12. [Decisions, risks, and references](11-decisions-and-risks.md)
13. [LLM and agent readiness](12-llm-readiness.md)
14. [Clean code, repository structure, and legacy deletion](13-clean-code-structure-and-deletion.md)
15. [Plan integrity and requirements traceability](14-plan-integrity-and-traceability.md)
16. [Agent execution guide and work packages](15-agent-execution-guide.md)
17. [Required artifacts and specification backlog](16-required-artifacts-and-specification-backlog.md)

## Non-negotiable invariants

1. Provider code never leaks into the domain or application crates.
2. A provider can be replaced without rebuilding `aseman-node`.
3. The node-facing VMM runs outside `aseman-node` and always exposes the same authenticated HTTP contract; reference VMM backends are independently replaceable behind it.
4. Every persistent datum is a logical capsule, regardless of its physical database representation.
5. SQL providers use native schemas: each core entity type has its own table.
6. VMs never receive database credentials and cannot choose their trusted creature, database, namespace, or provider role; they authenticate signed requests to the Aseman proxy with registered workload keys.
7. All VMs belonging to programs of the same creature resolve to that creature's dedicated guest database/namespace and provider role, where the creature may manage multiple tables or collections within policy.
8. VMs belonging to different creatures cannot connect to, read, enumerate, modify, infer, or inspect the catalogs of each other's guest databases.
9. Every sensitive action is authorized, including discovery, logs, terminal, signals, network access, and resource creation.
10. A child workload can receive only an attenuation of delegable parent authority plus administrator policy.
11. Cross-node operations are authenticated and reauthorized by the destination/home node.
12. Resource charges are derived from measured usage and settle idempotently.
13. Provider activation is validated, health-checked, drainable, and rollback-safe.
14. Database changes use staged migration; a configuration toggle must never cause silent data loss or weaker consistency.
15. Current, target, legacy, and generated documentation are explicitly distinguished and mechanically checked.
16. A migrated capability has exactly one authoritative implementation; replacement is not complete until the old path is removed or placed in an expiring compatibility package.
17. Every stated requirement has a design owner, delivery phase, acceptance proof, rollback path, and traceability entry.
18. No implementation depends on undocumented session context; missing exact schemas, inventories, fixtures, or decisions are registered artifacts with blocking phase gates.

## Execution policy

Use a strangler migration. Establish contracts and characterization tests around working behavior, move one responsibility behind a contract, verify parity, and only then remove the legacy path. Every phase in [09-migration-phases.md](09-migration-phases.md) has an exit gate; later phases must not depend on unverified behavior from earlier ones.
