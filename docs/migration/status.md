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
Nomad, and Firecracker, not doubles. Phases 0 through 8 are accepted. Phases 9 and 10
are recorded as **partial**, honestly, because their criteria are observable outcomes
(a clean host bootstrapping in one command; every acceptance criterion passing) and
those have not been observed. What remains is mostly delivery — serving the public
contract over HTTP, packaging, and the load and fuzz suites — not design.

## Phase gates

| Phase | State | Gate |
|---|---|---|
| 0 Audit | ACCEPTED | `phase-0-gate.md` |
| 1 Workspace and contracts | ACCEPTED | `phase-1-gate.md` |
| 2 Module system | ACCEPTED | `phase-2-gate.md` |
| 3 Capsule storage | ACCEPTED | `phase-3-gate.md` |
| 4 Security and authority | ACCEPTED | `phase-4-gate.md` |
| 5 Extract the native VMM | ACCEPTED | `phase-5-gate.md` |
| 6 Nomad and worker topology | ACCEPTED | `phase-6-gate.md` |
| 7 HTTP, federation, realtime | ACCEPTED | `phase-7-gate.md` |
| 8 Finance and metering | ACCEPTED | `phase-8-gate.md` |
| 9 CLI, packaging, operations | **PARTIAL** | `phase-9-gate.md` |
| 10 Hardening and rollout | **PARTIAL** | `phase-10-gate.md` |

## What is left, in rough order of leverage

1. **Serve the public contract.** `contracts/public/openapi.json` is generated and
   authoritative; the hardened HTTP stack, its middleware, and the SSE and WebSocket
   streams are not written. This also unblocks the federation transport, RL-004, and
   RL-009.
2. **`asemanctl` and packaging** (RL-015, RL-018). The bootstrap rules exist; the
   command and the images do not. Tracked `dist/*` blobs stay until there is a
   signed-artifact pipeline to replace them — deleting them first would be half of a
   two-part gate.
3. **The Hashgraph adapter** behind `ConsensusProvider` (RL-011). The port and its
   epoch rules are delivered; finance already depends on no consensus type.
4. **Load, fuzz, and full chaos suites.** The chaos cases the phase gates required are
   implemented and pass; the broader suites are not written.
5. **The remaining legacy deletions.** Every one is a row in the removal ledger with its
   blocker named, and `scripts/check_removal_ledger_due.py` fails a release if one goes
   quiet.

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
