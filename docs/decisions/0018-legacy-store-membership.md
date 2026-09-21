---
status: DECISION
owner: storage/security
source_of_truth: this ADR
last_verified_commit: f6be364d6761
verification: A304 core mapping plus A308 membership transforms and tests; membership_audit_repairs_ld12_residue_only_under_the_approved_digest
---

# ADR 0018: Store membership principals and legacy permission sets

## Status

Accepted 2026-09-21. Revises the A304 `core.store_membership` logical schema before any
implementation depends on it. P4 authorization consumes the result.

## Context

Legacy membership is a pair of links. `hasaccess::{member}::{store} = "true"` is what the
access guards check. `onaccess::{store}::{member}` holds a permission set, a
comma-separated subset of `read`, `signal`, and `manage`. Legacy writers always set or
delete the pair together.

A member can be a human or machine creature, a program (programs join stores and receive
signals), or a user of another federated node whose identity has no local row. The
accepted A304 schema relates a membership only to a local `core.user` and stores a
`role` that legacy never had. It could not represent legacy membership without dropping
members or inventing roles.

## Decision

`core.store_membership` is revised:

- The fields are `member_kind` (`creature`, `program`, or `remote_principal`),
  `member_ref` (the legacy member identity), `permissions` (the canonical encoding of the
  legacy permission set), and `joined_at_micros`.
- Relationships: `store` is required. `creature` or `program` is set exactly when the
  member resolves locally. A remote principal has no local foreign key; P7 federation
  identity resolves it later. `(store, member_kind, member_ref)` is unique.
- `role` is removed. Legacy authorization depends only on the permission set, and the
  set is preserved exactly: tokens may appear in any order, but every token must be
  known and none may repeat. The pre-permission literal `true` is preserved as the
  empty set, which legacy parses as deny-all.
- Legacy never recorded join time, so `joined_at_micros` is `0`, meaning unknown. The
  migration never fabricates a timestamp.
- `core.access_level` is unchanged. Legacy has no per-level configuration, so the
  migration produces none.

## Member resolution

The migration runner explicitly declares the legacy ID origins that belong to this
installation (legacy IDs are `{counter}@{origin}`). A member resolves in this order:

1. A local legacy creature, including machine creatures.
2. A local legacy program.
3. If the member's origin is declared local, it is a dangling member and the export
   fails closed.
4. Any other `x@origin` becomes a `remote_principal`. A member ID without an origin fails
   closed.

A migration that needs remote classification but has no declared local origins fails
closed. Guessing locality could turn a dangling local member into a trusted remote one.

## Fail-closed pairing

Every `onaccess` link needs its matching `hasaccess = "true"` and the reverse, and both
must name an existing store. A one-sided pair means legacy guards and signal delivery
already disagree, so it must be reconciled before export.

## Amendment 2026-09-21: audited pre-export repair (LD-12)

Legacy creature deletion never removed memberships (LD-12), so real installations hold
links the rules above refuse. The export stays strict. Instead, a separate step run by
an operator reconciles the data before export
(`audit_legacy_memberships` / `repair_legacy_memberships` in `aseman-storage-legacy`).
The audit applies this ADR's resolution rules and the export's derived-link checks. It reports:

| Defect | Repair |
|---|---|
| Membership in a store whose object is gone | Remove both links; the legacy store delete intends this |
| Dangling local member | Remove both links; the fixed legacy creature delete does this |
| One-sided pair (LD-11) | None. Deleting or completing it changes who can read or signal, so an operator decides |
| Store with no creator, or a dangling local creator | None. An operator reassigns or deletes the store |
| `ownerof` link to a missing creature, or naming an owner other than `ownerId` (LD-16) | Remove the link |
| Non-human creature without its derived `ownerof` link (LD-16) | Rebuild it from `ownerId`. With no `ownerId`, an operator decides |

The audit's digest is the approval token. The repair recomputes the audit from the
stopped node's keys and writes nothing unless the digest matches. It then deletes the approved links and rebuilds the approved derived links, in one
atomic batch, and reports what it removed and what still
needs a decision. The repair never runs automatically, and never guesses whether a
member is local.

## Rejected alternatives

- Mapping permission sets to owner/member/viewer roles: this loses the non-canonical sets
  legacy accepts.
- Dropping federated members until P7: this silently removes access that legacy grants
  today.
- Treating every unresolved member as remote: this launders dangling local references
  into external trust.
- Letting the export drop dangling memberships: this hides the data change from the
  operator. A separate step, approved by digest, makes the change explicit and
  auditable.
