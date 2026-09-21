# Decisions, Risks, and References

## Confirmed decisions

1. Aseman uses ports-and-adapters with out-of-process providers for runtime replacement.
2. The VMM boundary is versioned HTTP/OpenAPI with mTLS.
3. HTTP and PostgreSQL are the default network and database choices.
4. Nomad is the intended default VMM provider, pending licensing approval.
5. The legacy VMM is extracted as a supported native provider.
6. All persistent data uses the capsule protocol.
7. SQL core entities map to separate native tables.
8. Guest data uses a provider-native database/namespace and dedicated restricted role per creature. Signed workload requests authenticate to the Aseman proxy, which resolves and assumes that role; each creature may manage multiple tables/collections without a shared guest table.
9. Programs/VMs owned by the same creature share its guest-data namespace.
10. Hashgraph remains the initial consensus implementation behind a provider contract.
11. Nomad's internal topology is hidden behind one Aseman federation identity.
12. Database/provider switches are staged migrations, not unsafe instantaneous toggles.
13. Documentation and agent instructions are layered, executable where possible, status-labelled, and tested for drift.
14. Every migration uses a replacement gate followed by a deletion gate; compatibility code is isolated and expires.
15. ADR 0001 (`docs/decisions/0001-creature-isolated-guest-databases.md`) governs guest-data tenancy, proxy authentication, provider roles, and multi-table/collection schemas.
16. Nomad is integrated but not bundled, mirrored, downloaded, or redistributed without separate legal approval; operator-provided Nomad is selected explicitly (ADR 0002).
17. Module control and non-HTTP provider contracts use protobuf/gRPC with major-version and capability negotiation (ADR 0003).
18. Caspar aliases expire after two Aseman minor releases and at least 180 days following the first stable replacement, subject to deletion gates (ADR 0004).
19. Capsules use deterministic CBOR and versioned SHA-256 integrity preimages (ADR 0005).
20. Capsule kinds declare one of the accepted consistency profiles; security, finance, bindings, idempotency, and outbox state cannot weaken to eventual consistency (ADR 0006).
21. The initial PostgreSQL provider supports majors 17 and 18; time-series extensions are optional capabilities (ADR 0007).
22. The initial policy provider is a deterministic typed Rust capability evaluator (ADR 0008).
23. Canonical identities use typed UUIDv7 entity IDs, Ed25519 keys/signatures, explicit epochs, and administrator-enrolled federation roots (ADR 0009).
24. Firecracker privileged operations run through the restricted worker agent (ADR 0010).
25. Stateful workloads use declared portability tiers; universal/live migration is not implied (ADR 0011).
26. OpenRaft is removed after its responsibilities migrate to capsule storage and fenced coordination (ADR 0012).
27. Production defaults to three stateless Aseman control replicas behind one endpoint with PostgreSQL fenced leases; compact mode uses one replica (ADR 0013/A014).
28. PostgreSQL durable realtime/outbox is the default; the in-memory provider is development-only (ADR 0014).
29. Schemaless legacy JSON documents migrate as subject-bound document capsules whose structured `document` field has no native column; derived splats are rebuilt and compared, never migrated (ADR 0016).
30. Legacy finance migrates as one reconciled, immutable `finance.legacy_record` epoch; P8 converts it to double-entry, and derived counters are verified, never migrated (ADR 0017).
31. Store membership records a typed principal (local creature, local program, or remote principal) and the exact legacy permission set; dangling local members fail closed (ADR 0018).
32. Legacy custodial private keys are verified against their creature and never exported; cutover requires ADR 0009 proof-of-possession enrollment (ADR 0019, RL-019).
33. Legacy ID counters are verified and never migrated, dead chain-callback state is dropped, and superuser flags fail closed for review (ADR 0020).
34. Legacy guest `dbOp` pairs migrate into each machine creature's isolated guest database in the reserved `_aseman_legacy_kv` table (ADR 0021).
35. Legacy observed VM runtime is not exported; the native-legacy VMM keeps it and P5 reconciliation rebuilds `core.workload`. Durable VM intent (gateway routes, alarms, resource stores/entities, entity configuration and artifacts) migrates as core capsules (ADR 0022).
36. Legacy secrets migrate as authenticated ciphertext and are re-wrapped with AAD in P4; login grants are dropped (ADR 0023).
37. Legacy bridge grants migrate by token digest with topic claims (ADR 0024).
38. The legacy Hashgraph store remains consensus-provider state with a block-digest checkpoint (ADR 0025).
39. The Phase 3 cutover routes whole port families to one authoritative provider. Balances and the finance ledger stay on the legacy provider until P8, VMM observed runtime until RL-013, and cross-provider actions write capsules first and compensate on a failed legacy commit (ADR 0026).

## Resolved blocking ADR set

| Decision | ADR |
|---|---|
| Guest database isolation and signed proxy | 0001 |
| Nomad licensing/distribution | 0002 |
| Module RPC/version policy | 0003 |
| Caspar compatibility duration | 0004 |
| Capsule encoding/integrity | 0005 |
| Capsule consistency profiles | 0006 |
| PostgreSQL versions/extensions | 0007 |
| Initial policy engine | 0008 |
| Identifiers, keys, and trust roots | 0009 |
| Firecracker privilege boundary | 0010 |
| Stateful portability | 0011 |
| OpenRaft removal | 0012 |
| Control-plane HA/fencing (A014) | 0013 |
| Durable realtime default/topology | 0014 |

New evidence may amend an ADR but may not be used to bypass its dependent gate.

## Principal risks and mitigations

### Big-bang rewrite

Risk: feature loss and an untestable cutover.  
Mitigation: strangler sequence, characterization tests, provider parity, dual paths, and removal only after exit gates.

### Lowest-common-denominator storage

Risk: every database becomes an inefficient JSON/KV store.  
Mitigation: logical capsules plus provider-native schemas; separate SQL tables/collections for core kinds and capability negotiation.

### Cross-creature guest-data leakage

Risk: caller-controlled prefixes, IDs, filters, caches, or cursors escape namespace boundaries.  
Mitigation: signed workload authentication; trusted workload-to-creature database/role resolution; dedicated provider databases/namespaces and roles; catalog isolation; transaction-scoped role assumption; pool contamination tests; signed cursors; and adversarial conformance tests.

### Guest database and role explosion

Risk: one database/namespace and role per creature can exhaust provider catalogs, connection pools, file descriptors, or operational limits.  
Mitigation: providers declare tenant/namespace limits; pools are bounded, lazy, database-partitioned, and idle-evicted; provisioning is asynchronous and idempotent; capacity tests and quotas block unsafe activation.

### Proxy role confusion

Risk: connection reuse, cancellation, or a caller-controlled database name leaves a session operating under another creature's role.  
Mitigation: server-side bindings only, database-partitioned pools, transaction-scoped role assumption, mandatory reset/verification, deny-on-ambiguity behavior, and adversarial failure tests.

### Provider semantic mismatch

Risk: pause/resume, transactions, consistency, queries, or logs differ silently.  
Mitigation: capability declarations, mandatory behavior profiles, activation refusal, and shared conformance suites.

### Split-brain desired/observed state

Risk: node and VMM disagree after failure.  
Mitigation: durable operation records, idempotency, generations, reconciliation, and one authoritative owner for each state category.

### Double billing

Risk: retries or late samples create duplicate charges.  
Mitigation: stable interval/sample identity, append-only usage capsules, deterministic pricing, and idempotent double-entry settlement.

### Excess module privilege

Risk: a provider becomes an unrestricted path to hosts or data.  
Mitigation: process/OCI isolation, declared permissions, mTLS identity, least-privilege secrets/mounts, signed artifacts, and audited worker agents.

### Nomad licensing

Risk: Nomad Community Edition is source-available under the Business Source License rather than unconditionally OSI-open-source, which may conflict with distribution goals.  
Mitigation: ADR 0002 preserves the provider integration but prohibits bundling, mirroring, automatic download, or redistribution without separate legal approval. This document is not legal advice.

### Documentation and agent-context drift

Risk: prose, route lists, runtime lists, examples, and agent instructions describe different revisions, causing unsafe automated changes.  
Mitigation: single-source registries, generated references, explicit current/target/legacy status, scoped `AGENTS.md`, link/example checks, and repository-comprehension evaluations.

### Permanent dual architecture

Risk: strangler migration adds clean modules but leaves the monolith, duplicate algorithms, legacy protocols, and old dependencies active indefinitely.  
Mitigation: removal ledger, singular ownership, two-part replacement/deletion gates, deprecation deadlines, forbidden-dependency checks, and release failure for overdue compatibility paths.

### Mechanical cleanup damage

Risk: deleting code based only on text search, coverage, or an unused-dependency tool removes behavior reached through generated registration, features, or runtime dispatch.  
Mitigation: characterization tests, supported feature matrix, route/capability inventories, caller analysis, canary observation, and rollback commits precede deletion.

### Triple consensus or ownership confusion

Risk: Hashgraph, OpenRaft, and Nomad Raft are incorrectly treated as interchangeable.  
Mitigation: explicit ownership: Nomad Raft manages Nomad infrastructure state; Hashgraph provider handles financial ordering; Aseman application data uses its selected storage guarantees. Retain OpenRaft only with a documented non-overlapping role.

## Authoritative technical references

- Rust Cargo workspaces: <https://doc.rust-lang.org/cargo/reference/workspaces.html>
- Rust trait dyn compatibility: <https://doc.rust-lang.org/reference/items/traits.html>
- Nomad architecture: <https://developer.hashicorp.com/nomad/docs/architecture>
- Nomad production requirements: <https://developer.hashicorp.com/nomad/docs/deploy/production/requirements>
- Nomad workload identity: <https://developer.hashicorp.com/nomad/docs/concepts/workload-identity>
- Nomad task drivers: <https://developer.hashicorp.com/nomad/docs/deploy/task-driver>
- Nomad task-driver plugins: <https://developer.hashicorp.com/nomad/plugins/author/task-driver>
- Nomad allocation statistics API: <https://developer.hashicorp.com/nomad/api-docs/client>
- Nomad allocation APIs: <https://developer.hashicorp.com/nomad/api-docs/allocations>
- Nomad `raw_exec` warning: <https://developer.hashicorp.com/nomad/docs/job-declare/task-driver/raw_exec>
- Nomad QEMU driver: <https://developer.hashicorp.com/nomad/docs/job-declare/task-driver/qemu>
- Nomad federation: <https://developer.hashicorp.com/nomad/docs/architecture/cluster/federation>
- Nomad Community Edition licensing: <https://developer.hashicorp.com/nomad/docs/ce-license-support>
- Nomad license text: <https://github.com/hashicorp/nomad/blob/main/LICENSE>
- OpenAI Codex `AGENTS.md` guidance: <https://learn.chatgpt.com/docs/agent-configuration/agents-md>
- Rustdoc documentation tests: <https://doc.rust-lang.org/rustdoc/documentation-tests.html>
- Rustdoc intra-doc links: <https://doc.rust-lang.org/rustdoc/write-documentation/linking-to-items-by-name.html>
- Rustdoc documentation lints: <https://doc.rust-lang.org/rustdoc/lints.html>
- Cargo machine-readable metadata: <https://doc.rust-lang.org/cargo/commands/cargo-metadata.html>

## Change control

Material changes to invariants, boundaries, security, storage semantics, financial consistency, or migration order require an ADR and an update to this folder. Implementation pull requests must identify the requirement, phase/work package, exit gate, required artifacts/ADRs, affected contracts, migration/rollback path, tests, generated documentation, and removal-ledger entries they advance.
