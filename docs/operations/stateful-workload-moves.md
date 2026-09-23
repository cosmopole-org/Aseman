---
status: CURRENT
owner: operations
source_of_truth: ADR 0011, aseman-domain::volume (A605)
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain volume
---

# Moving a stateful workload

Written for: the operator moving workloads between workers, runtimes, or VMM providers.

Aseman does not promise live stateful migration. A move that crosses a worker, a
runtime, or a provider is refused unless every attached volume has a compatible declared
path. That refusal is the feature: the alternative is discovering afterwards that a
workload came back empty.

## Declare a tier for every volume

| Tier | What it means |
|---|---|
| `ephemeral` | Recreation may discard the data. It is never presented as migrated |
| `provider_local` | Movable only by its provider's own snapshot or export, and only within that provider |
| `portable_offline` | Quiesce, snapshot, checksum, copy, restore, verify, then start |
| `shared_external` | The data stays in a separately managed provider; the target must pass its attach and fencing checks |

Legacy volumes begin as `provider_local` until something proves otherwise (ADR 0011).
That is deliberately the most restrictive tier that still allows a move: an unproven
volume is not quietly treated as portable.

## Ask before you move

`plan_move(volumes, from, to)` answers with what the move would take, or refuses and
says which volume is in the way, by name. The plan is the strongest requirement any one
volume imposes — a single `portable_offline` volume among ephemeral ones still means a
quiesced offline copy.

It refuses a move between CPU architectures outright, even for a stateless workload: its
image is built for one architecture.

## Doing an offline copy

The order is not negotiable, and the source stays fenced throughout:

1. Quiesce the source, with a timeout. A source that will not quiesce does not move.
2. Snapshot it, and checksum the snapshot.
3. Copy it to the target, encrypted in transit.
4. Restore it, and verify the checksum at the target.
5. Start the target. **This is the irreversible point.**

Source and target are never writable at the same time. Before step 5, rolling back is
unfencing and resuming the source. After it, rolling back is a new migration in the
other direction — never an unfenced restart of the source, which would give you two
writers and no way to reconcile them.

## Recreating in place

`recreation_is_lossless(volumes)` says whether recreating a workload where it stands
keeps its data. `false` means recreation loses a `provider_local` or `portable_offline`
volume, and the recreation must be an operator's explicit decision rather than a
reconciler's.

## Legacy Modal volumes

`ModalVolume` handles from the Phase 3 export are `provider_local` and are not adopted
automatically. An operator adopts or explicitly releases each one; they are never
orphaned and never silently dropped (ADR 0022, removal ledger RL-005 and RL-013).
