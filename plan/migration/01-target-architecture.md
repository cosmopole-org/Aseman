# Target Architecture

## System shape

```text
Clients / administrators / VM guests / remote Aseman nodes
                             |
              HTTP + SSE/WebSocket + mTLS
                             |
                    aseman-node APIs
                             |
                 application use cases
                             |
             narrow, versioned logical ports
       +-----------+---------+---------+-----------+
       |           |         |         |           |
    storage     security  realtime   finance    federation
       |           |         |         |           |
       +-----------+---------+---------+-----------+
                             |
                       VMM HTTP client
                             |
                        aseman-vmm
                             |
             Nomad / native / future provider
                             |
                     VMM worker agents
```

## Proposed workspace

```text
apps/                 executable composition roots
  aseman-node
  asemanctl
  aseman-vmm
  aseman-vmm-agent
  aseman-meter

crates/               reusable in-process Rust libraries
  aseman-domain
  aseman-ports
  aseman-application
  aseman-contracts
  aseman-config
  aseman-observability
  aseman-module-runtime
  aseman-capsule
  aseman-guest-sdk

modules/              independently deployable provider packages
  storage/{postgres,rocksdb-legacy}
  network/{http,legacy}
  federation/http
  realtime/{durable,memory}
  security/capabilities
  finance/ledger
  consensus/hashgraph
  coordination/postgres
  vmm-backend/{nomad,native-legacy}
  runtime/{docker,firecracker,wasm,javascript}

contracts/            source OpenAPI/protobuf/schema definitions
deploy/               deployment assets and profiles
docs/                 architecture, concepts, ADRs, runbooks
examples/             executable examples
tests/                contract, integration, E2E, chaos suites
evals/agent/          comprehension evaluations
xtask/                deterministic repository automation
```

## Dependency rules

1. `aseman-domain` contains identifiers, entities, value objects, state machines, capability semantics, money, usage, and domain events. It performs no I/O.
2. `aseman-ports` owns narrow behavioral interfaces required by application use cases and uses domain values.
3. `aseman-application` owns use cases, orchestration, sagas, authorization calls, and desired-state reconciliation.
4. `aseman-contracts` owns stable wire DTOs generated from source OpenAPI/protobuf/schema definitions, errors, compatibility rules, and conformance fixtures.
5. In-process adapters implement ports; independently deployable modules implement wire contracts. Application and domain crates import neither.
6. Binaries perform configuration and dependency injection; they do not contain business rules.
7. Concrete database, transport, queue, consensus, and runtime types do not cross their boundary.
8. Internal APIs use typed identifiers and results, not unstructured `serde_json::Value` envelopes.
9. Libraries use typed `thiserror` errors; `anyhow` is limited to binary composition and top-level reporting.
10. Dependency cycles and undeclared boundary exceptions fail CI.

## State ownership

- Aseman owns users, creatures, policies, desired workloads, federation identity, finance, and durable operation state.
- A VMM owns observed allocation/runtime state and reports it through the VMM contract.
- A database provider owns physical representation and provider-native role enforcement; capsules, guest schema definitions, and creature-to-database bindings remain portable Aseman state.
- Each creature owns one logical guest database/namespace. Aseman's signed guest-data proxy resolves workload identity to a dedicated provider role, while workloads receive no database credentials and cannot select a database or role.
- A realtime provider transports events but is not the source of truth.
- A finance consensus provider orders/finalizes records but does not own pricing or wallet business rules.

Cross-service work uses idempotent commands, durable outboxes, operation records, and reconciliation. It does not use distributed database transactions.

## Node-cluster topology

An Aseman node is one logical federation identity even when internally replicated. Compact mode runs one Aseman control process and one Nomad server/client on one machine. Production mode may run multiple stateless Aseman API/control replicas behind one stable endpoint, three/five Nomad servers, and arbitrary Nomad clients.

Shared capsule storage holds authoritative control state. Singleton work such as reconciliation, outbox dispatch, directory publication, and scheduled settlement uses a provider-neutral `CoordinationPort` with expiring fenced leases. PostgreSQL is the default coordination provider; alternate providers must pass linearizability, fencing, failover, and clock-skew conformance tests. A lease holder's fencing token is checked on committed effects so a paused former leader cannot resume as a second authority.

Federation observes one node ID, endpoint set, and key epoch rather than individual replicas. Losing a control replica or worker must not change that identity.

## Compatibility and naming

- Rename public concepts from Caspar to Aseman early.
- Keep `caspar-node`, `casparctl`, `CASPAR_*`, and old protocol aliases as isolated
  deprecated shims for the ADR 0004 window: two Aseman minor releases and at least
  180 days after the first stable replacement, whichever is longer.
- Provide configuration and data migration commands.
- Record removal dates and emit actionable warnings.

## Observability

All modules emit structured traces, metrics, and logs with common node, creature, workload, operation, request, and settlement identifiers. Health is divided into liveness, readiness, and dependency health. Telemetry export remains provider-independent.

## Architecture as an agent-readable contract

The root workspace, typed contracts, dependency rules, source registries, and generated inventories form a machine-readable architecture map. Every crate/module documents purpose, state ownership, allowed dependencies, invariants, entry points, and targeted checks. Current behavior and migration targets are never mixed without explicit status labels. The complete requirements are in [12-llm-readiness.md](12-llm-readiness.md).
