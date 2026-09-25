---
status: DECISION
owner: security/guest
source_of_truth: this ADR
last_verified_commit: 5c6e6eb
verification: node guest_state tests; storage-legacy confined_guest_documents test; guest KV conformance
---

# ADR 0028: Guest JSON and link host calls are confined to the calling creature

## Status

Accepted 2026-09-22 (approved by the project owner). It fixes LD-24 and extends
ADR 0021.

## Context

Guest programs store their own document state with the `putJson`, `getJson`,
`getByPrefix`, and `delKey` host calls (for example `modules/runtime/javascript/examples/counter.js`),
and read values with `getLink`. The host served these calls against arbitrary node keys.
A guest could therefore read or overwrite anything in the node store: finance records,
sessions, secrets, custodial private keys (`link::UserPrivateKey::*`), and core
documents (LD-24). Two routes reached them:
- the unified host call, whose caller identity could itself be forged (LD-27);
- the Go-era `vm_callback` protocol, which carries no caller identity at all.

The alternative, removing the calls (`raw_state.*` = `never`), would break every guest
program that stores documents.

## Decision

1. **Confinement.** The calls exist only in `apps/aseman-node/src/drivers/vmm/guest_state.rs`, for a
   creature the node supplies. That creature is either the packet identity stamped by
   the runtime or the docker gateway, or the VM context registered for a runtime
   transaction. The guest's key only extends that creature's prefix:
   - Documents live at `GuestDoc::{creature}::{key}` in the JSON store, with the legacy
     layout: the object at each path, plus one record per nested object and non-null
     leaf.
   - `getByPrefix` lists only the creature's records, in the guest's own key space
     (`{key}::{path}`).
   - `delKey` without a path deletes every record of the document. Legacy deleted a raw
     key that holds no document records, so nothing was deleted; this is a behavior
     change.
   - `getLink` reads only the creature's own `dbOp` pairs (`{creature}::{key}`).
2. **No identity, no state.** A call without a trusted creature is refused.
   `vm_callback` refuses all five calls.
3. **Migration.** Each `json::GuestDoc::{creature}::{rest}` record becomes a
   `guest.legacy_kv` capsule in the creature's guest database, in the new `json`
   namespace. Its key is `{rest}`, which is `{key}::{path}`, and its value is the
   record's JSON text. A document of a creature that is not local fails closed. The
   reserved table's namespace constraint gains `json`, and tables created before this
   decision are upgraded idempotently.
4. **After cutover.** The guest gateway serves the same five operations over those rows
   with the same semantics (A405), so guest programs see no difference.

## Consequences

- Documents that guests wrote before this change, at unconfined keys, are no longer
  reachable by guests. The A308 export already fails closed on unreviewed families, so
  such documents surface in the export review rather than being silently attributed to
  a creature.
- The registry keeps `raw_state.*` as `never`. The guest calls are the confined
  `guest_data.access` operations.

## Rejected

- Removing the calls, which breaks existing guest programs.
- Keeping unconfined access for "trusted" runtimes: no guest code is trusted.
