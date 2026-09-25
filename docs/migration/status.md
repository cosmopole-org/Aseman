---
status: CURRENT
owner: migration
source_of_truth: the phase gate documents and docs/migration/acceptance.md
last_verified_commit: eebb9c5
verification: cargo xtask fast
---

# Where the migration stands

Written for: whoever picks this up next, cold.

## The one-paragraph version

The architecture is built and proven against real infrastructure — PostgreSQL, Docker,
Nomad, and Firecracker, not doubles. Phases 0, 1, 2, and 4 through 8 are accepted.
Phase 3 is **ready, not switched**: every artifact is accepted and every core family
reads and writes through ports with conformance-tested adapters, but the operator
cutover to `ASEMAN_CORE_STORAGE_PROVIDER=postgres` has not been performed. Phases 9 and
10 are recorded as **partial**, honestly, because their criteria are observable outcomes
(a clean host bootstrapping in one command; every acceptance criterion passing) and
those have not been observed. What remains is mostly delivery — serving the public
contract over HTTP, packaging, and the load and fuzz suites — not design.

## Phase gates

| Phase | State | Gate |
|---|---|---|
| 0 Audit | ACCEPTED | `phase-0-gate.md` |
| 1 Workspace and contracts | ACCEPTED | `phase-1-gate.md` |
| 2 Module system | ACCEPTED | `phase-2-gate.md` |
| 3 Capsule storage | **IN_PROGRESS** | `phase-3-gate.md` |
| 4 Security and authority | ACCEPTED | `phase-4-gate.md` |
| 5 Extract the native VMM | ACCEPTED | `phase-5-gate.md` |
| 6 Nomad and worker topology | ACCEPTED | `phase-6-gate.md` |
| 7 HTTP, federation, realtime | ACCEPTED | `phase-7-gate.md` |
| 8 Finance and metering | ACCEPTED | `phase-8-gate.md` |
| 9 CLI, packaging, operations | **PARTIAL** | `phase-9-gate.md` |
| 10 Hardening and rollout | **PARTIAL** | `phase-10-gate.md` |

## What is left, in rough order of leverage

1. **The Phase 3 storage cutover.** Every Phase 3 artifact is accepted and every core
   family reads and writes through ports with conformance-tested adapters; the remaining
   step is the operator action: run A309 verification, configure the guest proxy, and
   switch `ASEMAN_CORE_STORAGE_PROVIDER=postgres` (runbook step 7). Until it runs,
   R10–R13 stay PARTIAL in the requirements traceability report.
2. **Compose the node and stream the public contract.** `contracts/public/openapi.json` is
   generated and authoritative; `modules/network/http` provides its hardened TLS/HTTP
   transport boundary; `aseman-public-service` composes the authenticated/authorized
   action service with durable idempotency (A401 + A402 + `ActionExecutor` seam +
   `PublicActionIdempotency`), and the durable idempotency store is real over
   PostgreSQL (P7-06). What remains is node composition: starting the listener, a
   `SessionDirectory`, and the RL-004 executor, then SSE and WebSocket streams. The
   canonical gateway RPC and listener handoff semantics are delivered by P7-07; their
   runtime servers remain part of this composition. The federation HTTP provider is
   now delivered independently; its node signer/verifier/executor composition remains
   alongside RL-004 and RL-009.
3. **Finish `asemanctl` and packaging** (RL-015, RL-018). The canonical
   `apps/asemanctl` crate now owns the command implementation and `casparctl` is a
   one-way warning compatibility shim, but several required administration groups, stable
   structured output, and executable deployment profiles remain incomplete. Separate
   least-privilege image definitions for node, VMM, meter, and Nomad backend now live
   under `deploy/images`. Tracked `dist/*` blobs stay until there is a
   signed-artifact pipeline to replace them — deleting them first would be half of a
   two-part gate.
4. **Compose the Hashgraph financial epoch switch** (RL-011). The complete engine now
   lives in `modules/consensus/hashgraph`; its `HashgraphConsensusProvider` sends
   records through the real Babble proxy and derives finalizations/checkpoints from
   committed blocks. The remaining work is composing this provider into the node's
   finance flow and observing a checkpointed switch on a live peer mesh.
5. **Load, fuzz, and full chaos suites.** The chaos cases the phase gates required are
   implemented and pass; the broader suites are not written and need a deployment.
6. **The remaining legacy deletions.** Every one is a row in the removal ledger with its
   blocker named, and `scripts/check_removal_ledger_due.py` fails a release if one goes
   quiet.
7. **Retire superseded roots after their gates.** The generated
   `docs/generated/repository-layout.md` compares the plan to the tree. Every planned
   target package path now exists and owns implementation source. Eight legacy roots
   remain behind compatibility, replacement, or publication gates; path completion is
   not treated as permission to delete them.

Delivered in this pass: the requirements traceability report (A1005) runs in
`cargo xtask fast` and records 25 requirements, 0 violations, 12 MET, 13 PARTIAL, 0
OPEN. The repository-structure deviations from the plan's proposed hierarchy are
documented below.

## How to check the state yourself

```text
cargo xtask fast     # architecture, generated-artifact freshness, tests, clippy
cargo xtask full     # plus the legacy node's own suite and the native backend
```

The live suites need real services and skip without them:

```text
ASEMAN_TEST_POSTGRES_URL=...   PostgreSQL: storage, identity, coordination,
                               realtime, federation, finance, two-cluster federation
ASEMAN_TEST_NOMAD_ENDPOINT=... Nomad: the Nomad backend and worker lifecycle
                               (Docker also required)
```

The worker agent's suite skips without a `firecracker` binary. No suite fakes a service
it could have used for real.

## Reading order for a cold start

`plan/migration/15-agent-execution-guide.md`, then `docs/migration/acceptance.md` for
what is and is not done, then the gate document of whatever phase you are touching, then
its work units in `docs/migration/work-units/`, then the ADRs it names.

## Repository structure vs. the plan's proposed hierarchy

`plan/migration/01-target-architecture.md` and `13-clean-code-structure-and-deletion.md`
propose a **target** hierarchy. The current tree deliberately differs in these ways:

- **The planned nested capability packages now own their implementations.** Storage,
  network, federation, realtime, security, finance, consensus, VMM backends, and
  runtime drivers are at the paths in plan 01. Older auxiliary modules such as
  `guest-http`, `identity-native`, `public-service`, and the VMM protocol crates remain
  one level deep because they are additional adapters, not substitutes for a missing
  planned package.
- **`crates/aseman-capsule` owns the capsule repositories.** The capsule protocol lives
  in `aseman-contracts::capsule`; no compatibility repository crate proxies it.
  `aseman-guest-sdk` now owns workload-bound signed request construction without any
  caller-selected tenancy API.
- **All planned `apps/` roots own their implementations.** The `caspar-node`,
  `caspar-keygen`, and `casparctl` ADR-0004 warning aliases are binary targets inside
  the canonical app packages; no proxy package or reverse dependency remains.
  `apps/aseman-meter` is independent and composes A501 collection with PostgreSQL
  usage, pricing, and ledger ports.
- **Legacy roots were consolidated without proxy crates:** the client is
  `apps/aseman-client`, deployable samples are `examples/creatures`, all seven runtime
  implementations plus their compatibility SDK are `modules/runtime`, the generated
  registry belongs to the native backend, and old wiki pages are explicitly archived
  in `docs/legacy/caspar`. Tracked `dist/` and root scripts remain open under RL-016/18.
- **The required ownership roots now exist.** `deploy/` points to the authoritative
  topology contract, `examples/` defines the executable-example policy, and
  `evals/agent/` contains a mechanically checked cold-start catalog. Root
  `ARCHITECTURE.md`, `CONTRIBUTING.md`, `SECURITY.md`, `CHANGELOG.md`, and the
  `docs/` portal are present. Actual compact orchestration, broader generated SDK
  examples, and deployment-backed evaluation runs remain Phase 9 work.

In short: the plan's hierarchy is the target; the tree is the strangler's current edge.
The dependency boundaries the plan actually enforces (`aseman-domain` ← `aseman-ports`
← `aseman-application`, modules behind contracts) are checked by `cargo xtask arch` and
are met. The exact structural differences are generated from
`contracts/repository/layout.json`; the missing/renamed entries are each a named
removal-ledger or Phase 9/10 obligation.
