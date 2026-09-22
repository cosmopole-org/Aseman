---
status: ACCEPTED
owner: security/guest
source_of_truth: this contract, ADR 0001, ADR 0021, contracts/capsule/guest/isolation-rules.json
last_verified_commit: 5c6e6eb
verification: cargo test -p aseman-application guest; live aseman-storage-postgres live_guest_gateway
---

# A405: guest gateway resolution and authorization (v1)

A workload reaches exactly one guest database: its own creature's. Nothing the guest
sends selects the creature, program, database, namespace owner, or role.

## Authentication

The gateway serves an authenticated workload, established in one of two ways:
- **Signed request.** An A401 proof in the `request` context, signed by the workload's
  `authentication` key. `subject` is `workload:{uuid}`, `audience` is
  `node:{node uuid}/guest/v1`, and `action` is the registered action of the operation.
  The body digest covers the exact operation body. All A401 checks apply, including
  replay.
- **In-process runtime.** The VM handle the node registered when it started the
  workload (the node-side VM context). A `vmId` or any other identity carried in the
  guest's input is never used.

## Resolution

Each link is resolved server-side from trusted records, and the gateway refuses at the
first link that fails:

1. **Workload**, from `core.workload`: it must exist and not be `deleted`.
2. **Program and creature**, from the workload's relationships. The program's own
   `creature` relationship must equal the workload's creature; otherwise the lookup
   fails closed.
3. **Binding**, from `core.guest_database_binding`, keyed by the creature. It must be
   `active`. Generations never move backwards.
4. **Provider check.** The guest provider re-derives the database and role names from
   the creature and generation before any connection. A catalog record that names
   another creature's database cannot route.

## Authorization

The operation is `guest_data.access` on resource `guest_data:{creature}`, decided by the
policy provider (A404) with the fact `same_creature`. That fact holds by construction,
because the resource was derived from the authenticated workload. The signed `action`
must equal the operation's action.

## Operations (v1): legacy key/value, ADR 0021

Operations are `get`, `put`, `delete`, and `list(prefix, limit)`, in the `dbop` or
`applet_db` namespace of the creature's reserved `_aseman_legacy_kv` table:
- Keys are 1 to 1024 bytes.
- Values are at most 1 MiB.
- A list returns at most 1000 pairs.

Every row is a sealed `guest.legacy_kv` capsule:
- A write is the pair's next revision. Migrated pairs keep their identity; new keys
  get a UUIDv7.
- A delete is its tombstone revision. Unlike legacy, deletes really delete.
- A list returns committed pairs by exact prefix (`starts_with`, no wildcards), in byte
  order. Unlike legacy, listing sees committed pairs.

Every operation runs inside the creature's role, in a transaction whose identity is
checked before and after (A306).

## Documents (ADR 0028)

`putJson(key, path, data, merge)`, `getJson(key, path)`, `delKey(key, path)`, and
`getByPrefix(prefix)` run over the `json` namespace, one row per legacy JSON record,
keyed `{key}::{path}`. `putJson` writes exactly the records the legacy `index_json`
wrote: the object at `path`, merged with the stored one when `merge` is set, then every
nested object and non-null leaf at `path.member`
(`aseman-contracts::legacy_documents::legacy_json_index_writes`).
- `getJson` returns the object stored at the path, or `{}`.
- `delKey` with an empty path deletes every record of the document; otherwise it
  deletes the record at the path and its subtree.
- `getByPrefix` lists record keys in byte order.
- A document root must be an object.

## Refusals

Refusals are stable reasons:
- `only workloads use the guest API`
- `the credential does not cover this operation`
- `the operation exceeds the guest limits`
- `unknown workload`
- `the workload is deleted`
- `the creature's guest database is not active`
- `the policy denies guest data access`

A failing proof returns its A401 code. A store that cannot answer is reported as
unavailable, and nothing is served.
