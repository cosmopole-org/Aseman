---
status: ACCEPTED
owner: security/guest
source_of_truth: this contract, ADR 0001, ADR 0021, ADR 0030, contracts/capsule/guest/isolation-rules.json
last_verified_commit: eebb9c5
verification: cargo test -p aseman-application guest; live aseman-storage-postgres live_guest_gateway; modules/vmm-backend/native-legacy/tests/system.rs (host calls from a real runtime)
---

# A405: guest gateway resolution and authorization (v1)

A workload reaches exactly one guest database: its own creature's. Nothing the guest
sends selects the creature, program, database, namespace owner, or role.

## Authentication

The gateway serves an authenticated workload, and the only way to be one is an A401
proof signed by the workload's `authentication` key. `subject` is `workload:{uuid}`,
`audience` is `node:{node uuid}/guest/v1`, and `action` is the registered action of the
operation. The body digest covers the exact operation body. All A401 checks apply,
including replay. The proof travels in the `request` context, or in the `Aseman-Proof`
header for a host call.

Who holds the key depends on where the workload runs, and the gateway cannot tell them
apart, which is the point:
- **Out-of-process.** The workload itself holds the credential it was bootstrapped
  with.
- **In-process runtime.** The VMM backend hosting the runtime holds one credential per
  workload it runs and signs on its behalf.

Since P5-06 there is no unauthenticated in-process path: the node registers no VM
handles and serves no callback. A `vmId`, `programId`, or any other identity carried in
the guest's input never establishes the caller (LD-14, LD-27).

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

## Host calls

The guest API is also the only way a workload reaches a node capability. A host call is
`POST /guest/v1/calls/{op}` with the call's JSON input as the body; the artifact read is
`GET /guest/v1/artifacts/{digest}`. Both carry the proof in the `Aseman-Proof` header
(unpadded base64url of the proof JSON), because a guest runtime cannot always shape a
request context.

- **Registration.** `op` is served only when the A402 registry has the surface
  `unified-host-call {op}`; the signer and the verifier both derive the signed action
  from it (`aseman_contracts::guest_api::call_action`). An unregistered op is refused
  before anything is resolved.
- **Confinement.** The caller resolves to a workload, and from it to a program and a
  creature, exactly as above. Every entity a call reads or writes is confined to that
  creature; a call naming an entity of another creature is refused, not filtered
  (LD-26).
- **Targets.** A call whose subject is another VM or program (`runVm`, `execVm`,
  `deleteVm`, `deployEntity`, `updateProgram`, ...) carries the target in
  `targetVmId`/`targetProgramId`, never in `vmId`/`programId`: those name the caller.
  The node moves the target into place only after it has resolved and authorized the
  caller, so a guest can never impersonate one by naming it (LD-27).
- **VM operations.** A host call that operates on a VM is an A501 call to the workload's
  VMM. The node performs no runtime work of its own (ADR 0030).
- **Artifacts.** `ARTIFACT_ACTION` (`workload.artifact.read`) lets a workload read its
  own program's artifact by digest, and nothing else.

## Credential

A workload's credential is an Ed25519 seed with its subject, key id, key epoch, guest
API base URL, and audience, encoded as unpadded base64url JSON. It is the write-only
`bootstrap.credential` of A501: the VMM accepts it, hands it to the backend, and never
returns or logs it (ADR 0019, ADR 0023). It never appears in `Debug` output, in an
event, or in a capsule. A key epoch change invalidates it; the workload is restarted
with the next one.

## Refusals

Refusals are stable reasons:
- `only workloads use the guest API`
- `the credential does not cover this operation`
- `the operation exceeds the guest limits`
- `unknown workload`
- `the workload is deleted`
- `the creature's guest database is not active`
- `the policy denies guest data access`
- `unknown host call`
- `the entity belongs to another creature`

A failing proof returns its A401 code. A store that cannot answer is reported as
unavailable, and nothing is served.
