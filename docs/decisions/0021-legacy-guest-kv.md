---
status: DECISION
owner: storage/guest
source_of_truth: this ADR
last_verified_commit: f6be364d6761
verification: A308 guest KV transform tests; A306 provisioning at import
---

# ADR 0021: Legacy guest `dbOp` storage migrates into each creature's guest database

## Status

Accepted 2026-09-21. It applies ADR 0001 to the legacy guest key/value store.

## Context

The JavaScript and WebAssembly runtimes expose `dbOp` (`put`, `get`, `del`,
`getByPrefix`) to guest code. The host prefixes every guest key with the VM's machine
creature ID and commits it, directly (`vm_db_op`) or from the VM's write-ahead buffer
(`VmTrxBuffer::commit`), with `put_link`. The physical record is
`link::{machineId}::{guestKey}` in the shared application RocksDB. Guest keys and values
are UTF-8 strings taken from the call input. This is the legacy guest database: shared
physically, and separated only by a key prefix that the host controls.

Two legacy defects shape what a guest can observe:
- `del` calls `del_key("{machineId}::{guestKey}")` without the `link::` prefix, so a
  committed guest key is never deleted.
- `getByPrefix` scans raw keys, so it never returns committed pairs, only the VM's
  uncommitted buffer.

The observable committed state is therefore exactly the set of link values that `get`
returns.

## Decision

- Each legacy guest pair becomes a `guest.legacy_kv` capsule in the `GuestData` storage
  class. It is owned by the machine creature and has body `{namespace, key, value}`, all
  exact text. Its identity derives from `(namespace, machineId, guestKey)`.
- There are two legacy namespaces:
  - `dbop`: the runtime `dbOp` calls, committed as `link::{machineId}::{guestKey}`.
  - `applet_db`: the host-function `dbOp`, committed as `link::AppletDb::{dbPrefix}::{key}`.
    Its `dbPrefix` starts with the creature ID, or with the program ID when the call had
    no creature, in which case the program's machine creature owns it. Its key is the
    exact remainder after `AppletDb::`, so the P4-04 gateway can recompute it from the
    authenticated hierarchy.
  An `AppletDb` prefix that names no local creature or program fails closed.
- The target is the machine creature's own isolated guest database (ADR 0001), in the
  reserved, migration-owned table `_aseman_legacy_kv` (`namespace`, `key`, and `value`
  text; primary key `(namespace, key)`), which `contracts/capsule/guest/legacy-kv-table.json` defines. The `_aseman_`
  prefix is reserved, so guest schema mutations can neither create nor collide with it.
- Import obligation: the guest provider provisions or reuses the creature's binding and
  creates the reserved table before inserting its records. The export never carries
  database names, roles, or generations, because those are server-derived at import
  (ADR 0001).
- A link `{family}::{rest}` whose family segment is exactly a local legacy creature ID is
  guest KV. Creature IDs contain `@`, so they never collide with named link families. A
  non-UTF-8 value fails closed. A link whose family is neither reviewed nor a local
  creature stays `Unmapped`. Raw `{machineId}::{key}` records have no legacy writer and
  stay `Unmapped`.
- Every committed pair migrates, including pairs a guest tried to delete; the legacy delete
  never took effect, and `get` still returns them. The P4-04 gateway implements `del`
  and `getByPrefix` correctly from cutover, which is a documented behavior change
  (legacy characterization A008).
- The P4-04 guest gateway serves legacy `dbOp` calls from the reserved table through the
  authenticated workload-to-creature binding. The caller never selects the creature.

## Rollback

The export is additive. Until cutover the legacy RocksDB stays authoritative for guest
state, and rollback discards the imported rows.
