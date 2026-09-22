---
status: DECISION
owner: vmm
source_of_truth: this ADR, contracts/vmm/openapi.json, contracts/vmm/states.json
last_verified_commit: 5c6e6eb
verification: cargo test -p aseman-contracts vmm; cargo test -p aseman-domain vmm; python3 scripts/generate_vmm_parity.py --check
---

# ADR 0029: The node-to-VMM contract covers invocations, and the node owns desired generations

## Status

Accepted 2026-09-22. It freezes A501, A502, A503, and A505 for Phase 5 (P5-01).

## Context

Plan 04 lists the VMM endpoints: capabilities, workload CRUD and lifecycle, exec, logs,
events, usage, operations, health, and version. The legacy runtimes are mostly
event-driven, though. The node runs wasm, JavaScript, elpian, and elpify once per
signal (`run_vm`), pushes signals into live docker containers, forwards HTTP ingress,
copies files, builds images, runs chain transactions, and verifies elpify proofs (A006).
A contract with only the plan's list could not carry these, and the node would keep a
side channel to the runtimes. P5-06 forbids that side channel.

Two parties also write workload state. The node decides desired state, and the VMM
observes what runs. Without an ordering rule, a retried or reordered command could
undo a newer one after a crash.

## Decision

1. **A complete A501** (`contracts/vmm/openapi.json`, OpenAPI 3.1). Besides the plan's
   endpoints, it has:
   - invocations (`signal`, `chain_transactions`, `chain_effects`);
   - HTTP forwarding;
   - file copy in and out;
   - builds;
   - snapshots and restore;
   - endpoints;
   - proof verification;
   - an interactive terminal (WebSocket, `aseman.terminal.v1`);
   - a global event stream for reconciliation.

   Every legacy runtime operation and `IVmm` method has a recorded destination in
   `contracts/vmm/native-parity.json` (A505). The generator fails on any unmapped one.
2. **Common rules.** They are checked by the contract tests for every operation:
   - mutual TLS, except the health probes;
   - an `Idempotency-Key` on every mutation (24-hour retention, 422 on reuse with
     another body);
   - optional `If-Match` resource versions;
   - `X-Request-Id`, `traceparent`, and `Aseman-Deadline` propagation;
   - RFC 9457 problems with a closed code set, each with a fixed HTTP status;
   - cursor pagination on lists;
   - SSE with `Last-Event-ID` resumption, where `resync` tells a reader its position
     expired.
3. **Capability negotiation.** `RuntimeCapabilities` carries eleven flags and the
   deploy conventions. A VMM refuses an operation a runtime lacks with
   `unsupported_operation`, and never degrades it (for example pause to stop). Legacy
   `exec_vm` in wasm, JavaScript, elpian, and elpify was an echo or an invocation, so
   those runtimes declare `exec: false`. No native runtime pauses today, so none
   declares `pause`.
4. **Generations (A503).**
   - The node owns desired state. Every accepted desired change, state or spec, advances
     its generation by one, as compare-and-set on the generation the caller read.
   - Commands carry that generation. The VMM applies a newer one, replays an equal one
     idempotently, and refuses an older one with `stale_generation`, naming the current
     generation.
   - Observations carry the generation acted on and a provider sequence. They are
     accepted only when they move forward and do not claim a generation newer than
     desired.
   - Reconciliation takes one step at a time toward the current generation. An
     instance nobody desired is **adopted** as stopped for operator review, never run
     or deleted silently (ADR 0022).

   The rules are pure functions in `aseman-domain::vmm`. `contracts/vmm/states.json`
   is their published table, and a test fails when the two disagree.
5. **Wire types.** `aseman-contracts::vmm` holds the Rust types. A test requires every
   object schema to have a type whose members equal the schema's properties, so the
   two cannot drift. The workload credential is write-only: redacted in `Debug`, and
   never returned.

## Consequences

- The node needs no runtime knowledge beyond `/v1/capabilities`. P5-02 builds the
  service and client on these types, and P5-03 puts the native backend behind A504.
- The legacy terminal (`openVmTerminal`) was a log subscription. It maps to
  `streamWorkloadLogs?follow=true`. The interactive terminal is contract-only until a
  runtime declares `terminal`.
- Guest host calls are not VMM operations. They go to the guest gateway (P5-04).
