---
status: LEGACY
owner: compatibility/caspar
source_of_truth: docs/README.md
---

# Creature model & type registry

A **creature** is the general model for *every being* on the Caspar network that
can act — hold a balance, own resources, hold accesses, send signals. Humans are
creatures; the non-human machines that own programs are creatures too. There is
**no separate `User` or `Machine` model** — those are simply creature *types*.

## Base model

Every creature shares one fixed base, stored under `obj::Creature::{id}`:

| field | meaning |
|-------|---------|
| `id` | unique creature id (`{n}@{origin}`) |
| `type` | the creature's registered type (e.g. `human`, `machine`) |
| `username` | unique handle |
| `publicKey` | RSA public key — the identity the signed action protocol verifies against |
| `balance` | token balance (i64) — the single wallet |

`Creature` is the *sole* record and the single source of truth. Identity
(`publicKey`, `type`, `username`), the wallet (`balance`), and program ownership
(`ownerId`, chain/subchain ids) all live on it. Every balance-moving action
(`transfer`, `mint`, `lockToken`, `consumeLock`) reads and writes
`Creature.balance`, and every read (`/creatures/get`, `authenticate`, …) returns
it, so a live fetch is always correct. No mirrored rows, nothing to diverge.

## Extensible type registry

On top of the fixed base, the host managing Caspar registers **customized
creature types**. A type is just a named spec:

```json
{ "initialBalance": 1000000000000000, "customFields": [], "desc": "…" }
```

| spec field | effect |
|------------|--------|
| `initialBalance` | balance seeded when a creature of this type is created |
| `customFields` | additional declared fields `[{name, type, required, default}]` on top of the base |
| `desc` | human description |

Behaviour is driven by the creature's `type` field and its spec — there are no
ad-hoc boolean flags. Types are stored under `Json::CreatureType::{name}` and
enumerated through the `CreatureTypeExists::{name}` link index.
`/creatures/create` looks up the requested type to seed the initial balance
(with a built-in fallback for `human`/`machine` so the very first creature,
created before install runs, still works); unknown types are rejected.

### Built-in types

Registered idempotently in the shell API's **install (bootstrap) phase**
(`install_creature_types`, called from a namespace's `install()`):

- **`human`** — the primary being. `initialBalance = 0`.
- **`machine`** — a non-human being that owns programs. `initialBalance = 0`.
  ("Machines" in the program/deploy API are exactly the creatures of type
  `machine`; `/machines/list` filters creatures by `type == "machine"`.)

Registration is *register-if-absent*, so it is safe to run from every
namespace's install and to add further host-defined types the same way.
During install, nodes created with the former `human` default of `1e15` migrate
that legacy type spec to zero. Other host-defined initial balances are left
unchanged.

### Inspecting the registry

```bash
# POST /creatures/types
# -> { "types": [ { "name": "human", ... }, { "name": "machine", ... } ] }
```

## Minting: the only way tokens come into existence

`transfer`, `lockToken` and `consumeLock` redistribute balance that already
exists. `/creatures/mint` is the one action that *creates* it, so it is the most
restricted:

- **Only `1@global` may call it** — the first creature registered on the
  network, bootstrapped by `casparctl` on the first `run` after an install.
  Every other creature gets `access denied`.
- The target is resolved by `UserEmailToId::{toUserEmail}`, i.e. the email the
  creature was *registered* with at `/creatures/login`. That link is written
  once and never moves, so an address a caller keeps on its own side can drift
  out of date; `/creatures/checkSign` returns the authoritative one.
- `amount` must be positive, and the credit is checked for overflow.

```json
POST /creatures/mint
{ "toUserEmail": "payer@example.com", "amount": 10000000, "idempotencyKey": "topup_9f3c…" }
-> { "applied": true, "balance": 1000010000000 }
```

### `idempotencyKey` — applied at most once

Minted tokens cannot be clawed back, which leaves a caller that is interrupted
*between* a successful mint and recording that fact with no safe move: retrying
credits the payer twice, giving up credits them not at all.

Naming the payment closes the window. The node records `MintApplied::{key}` in
the same write batch as the balance, so the marker and the credit land together
or not at all. A later call with a key that has already been applied leaves the
balance untouched and answers:

```json
{ "applied": false, "alreadyApplied": true, "previous": "7@global:10000000" }
```

That is a **success**, not a failure — it means "this payment is already
minted", which is exactly what a retrying caller needs to hear before marking
its own record settled. The key is global and opaque to the node; use whatever
identifies the payment on the caller's side (Decillion's Nest backend sends its
top-up record id — see `decillionai-server/AGENTS.md`).

The field is optional for backwards compatibility, and a mint without one
behaves as before: applied unconditionally, every time it is called. Any caller
crediting real money should always send one.

Markers are kept forever — that is the point, since a retry can arrive
arbitrarily late — so the keyspace grows by one small link per payment minted.
Do not prune it: a dropped marker turns a late retry back into a double credit.
