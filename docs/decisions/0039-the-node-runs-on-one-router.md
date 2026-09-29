---
status: DECISION
owner: architecture
source_of_truth: this ADR; apps/aseman-node/src/actions/
last_verified_commit: pending
verification: cargo test -p aseman-node --lib actions::tests; cargo xtask fast; cargo xtask full
---

# ADR 0039: The node runs every operation through one router, and the migration is closed

## Status

Accepted 2026-09-29. Closes the Caspar-to-Aseman migration.

## Context

The node carried two ways to run the same operation. The signed-packet transports (TCP,
WebSocket, the chain, federation, and a guest's `execShellAction`) dispatched to
hand-registered action closures behind an actor registry, a secure-action wrapper, and
global singletons (`GLOBAL_APP`, the installed storage, the master key, the topic
registry, the audit sender, the guest-data routing). The public HTTP API (A701) ran a
second implementation of each action family in the `ActionExecutor` behind
`aseman-public-service`. The two diverged: HTTP `getByUsername` looked up by id,
`meta` wrote, `machines/list` returned every creature, `stores/signal` did not fan out,
`delete` left metadata behind, `checkSign` was missing, every HTTP failure read as
*unavailable*, and the HTTP caller was a subject UUID where the handlers expected the
creature's id.

The migration's plan, phase gates, removal ledger, and artifact register had done their
job: every capability had a replacement, and the remaining legacy paths were this
duplicate dispatch and the node's module layout inherited from the Caspar node.

## Decision

1. **One operation table.** `apps/aseman-node/src/actions/mod.rs` declares every
   operation once: its path, where a signed packet runs it (`Local`, `Replicated` on the
   main chain, or `Requested` on the node its `origin` names), and its typed handler.
   The router checks the table against the A402 registry when it is built: every
   registered shell surface has exactly one operation, and each operation carries the
   registry's action and packet guard.
2. **Every surface enters the router.** HTTP (`RouterExecutor`), the TCP and WebSocket
   sessions, a guest's `execShellAction`, the chain's ordered requests, and federation's
   forwarded requests all call `Router::execute` or `Router::dispatch`. The transports
   are framing only (ADR 0033); none reaches a handler directly.
3. **An operation is one transaction.** It commits on success and discards its writes
   on refusal (LD-15). A402 authorization runs inside the transaction for the
   signed-packet transports; HTTP is authorized by the public action service before it
   reaches the router.
4. **Signed packets are admitted by their operation's packet guard** (`public`, `user`,
   `store`, `finance`): a real signature, or a machine's applet marker from inside the
   node for `user` and `store`. A replicated operation is admitted on the node that
   received it *before* it is ordered on the chain, because every node runs an ordered
   packet as from inside.
5. **Typed errors end to end.** An input that is not the operation's input is
   `Invalid` (HTTP 400, packet code 2), a refusal is `Refused` (`PortError::Refused`,
   HTTP 422, packet code 3), and a storage failure is `Unavailable`.
6. **HTTP callers are creatures.** The router's executor resolves an HTTP subject to the
   creature whose record it names through the legacy-identity index, so handlers see
   the same caller on every surface.
7. **No global state.** The node's components are owned by `Node` (`node/`), and the
   storage, master key, topics, audit log, guest-data routing, router, and VMM client
   are reached through it.
8. **The layout names what the node does:** `actions/` (the router and the operation
   families), `transports/` (HTTP, storage HTTP, admin, shell, federation, chain),
   `live/` (the signal hub and topics), `workloads/` (the VMM client, guest host calls,
   ingress), `state/` (the models and their ports), `node/` (composition and
   accessors).
9. **The migration is closed.** `plan/migration/`, `docs/migration/` (status, phase
   gates, removal ledger, artifact register, call graph, data map, baselines), the
   archived Caspar documentation, and the scripts that generated or checked them are
   deleted; the git history keeps them. What code still cites is kept as reference:
   `docs/reference/artifacts.md` (the `A###` artifact catalog) and
   `docs/reference/defects.md` (the `LD-##` defects and their resolutions).
10. **Names describe the live system.** Crates and modules lose their migration names:
    `aseman-network-legacy` is `aseman-network-shell`, `sdk-legacy` is `aseman-vm-sdk`,
    the `caspar-vm-*` runtime crates are `aseman-vm-*`, the native backend is
    `vmm-backend/native` (described as `native`), and the contracts modules are
    `wire`, `documents`, `signals`, `vm_routes`, `storage_http`, and `creature_keys`.
    Configuration keys lose their `ASEMAN_LEGACY_` prefix (the chain port is
    `ASEMAN_CHAIN_PORT`, clear of the `ASEMAN_CONSENSUS_*` provider properties), and
    `contracts/config/retired-names.json` refuses the Caspar names.
11. **What keeps the word *legacy*.** It remains only where it names data or protocol
    that exists: the `<n>@<origin>` string ids and their persisted
    `core.legacy_identity` index (renaming them would be a data migration), the Caspar
    storage layouts that `asemanctl storage migrate` and the VM handoff import, persisted
    format tags (`legacy.store.signal`, `chacha20poly1305-legacy-v1`), and the runtimes'
    compatibility entry points for older creature SDK builds.

## Consequences

- A behavior change to an operation is made once, in its handler, and every surface
  sees it; `actions/tests.rs` exercises the families, the guard with real signatures,
  the dispatch codes, and the HTTP caller resolution on an in-memory node.
- Deployments that set `ASEMAN_LEGACY_*` keys must rename them to `ASEMAN_*`
  (`ASEMAN_LEGACY_CONSENSUS_PORT` becomes `ASEMAN_CHAIN_PORT`,
  `ASEMAN_LEGACY_CASPARCTL_*` becomes `ASEMAN_CTL_*`).
- ADRs 0001–0038 keep their plan references (`P5-03`, `RL-011`, …) as the historical
  record of why they were decided; those IDs resolve in the git history.

## Rollback

Revert the change set. No persisted data changed shape; the only operator-visible
change is the configuration key names.
