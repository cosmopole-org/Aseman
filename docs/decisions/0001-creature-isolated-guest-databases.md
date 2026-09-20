---
status: DECISION
owner: storage/security
source_of_truth: this ADR
last_verified_commit: 800df24076c794f9b33c29a9f301dd9d674f47d6
verification: plan traceability and storage/security conformance suites
---

# ADR 0001: Creature-isolated guest databases and signed proxy authentication

## Status

Accepted by product direction on 2026-09-19. Exact signature algorithms, canonical
request encoding, provider-specific role-assumption mechanisms, and the portable guest
schema subset remain specifications to complete in A301, A306, and A401.

## Context

The earlier migration plan placed every creature's guest records in one shared dynamic
table or collection and relied on a mandatory `creature_id` predicate plus provider
defenses such as PostgreSQL row-level security. That layout unnecessarily mixes tenants
and prevents a creature from managing a natural multi-table or multi-collection data
model.

Workloads must not receive database passwords or privileged provider connections. They
already have workload identities and public keys that can authenticate requests to an
Aseman-controlled proxy.

## Decision

Each creature receives one provider-native logical guest database or equivalent isolated
namespace and one dedicated, non-login provider role/principal restricted to that
database. All programs and workloads owned by the creature resolve to the same guest
database and role.

A workload opens a guest-data session through the Aseman proxy by signing a canonical,
audience-bound request or challenge with its registered workload key. The signed material
includes freshness and replay-protection fields. The proxy verifies the signature,
resolves workload -> program -> creature from authoritative state, and selects the
database and role from its server-side binding. Caller-supplied database, role, creature,
or namespace identifiers are never trusted for routing or authorization.

The proxy assumes the dedicated provider role for each operation or transaction. VMs do
not receive database credentials and do not connect directly to provider administration
interfaces. Pools are partitioned by provider/database and role assumption is
transaction-scoped, reset, and tested against identity leakage.

Within its database, a creature may create, alter, index, and remove multiple tables or
collections, subject to quotas, reserved names/fields, schema validation, retention,
provider capabilities, and administrator policy. It cannot create principals, escape its
database, inspect other tenant catalogs, install privileged extensions, or change server
configuration.

Capsule portability remains mandatory. A guest table/collection definition is a logical
capsule schema, and every guest record is a logical capsule. Providers may store capsule
metadata in protected columns/fields or provider-private sidecars; they must not force all
creatures or all guest kinds into one shared physical table/collection. Provider-specific
schema features are allowed only when declared; migrations must reject or explicitly
transform unsupported features rather than silently lose them.

## Provider mapping

- PostgreSQL defaults to a dedicated database plus a `NOLOGIN` creature role assumed by
  the proxy. Public database/schema privileges are revoked. The role owns or receives
  constrained DDL/DML rights only inside that database.
- Document databases use a dedicated database/namespace and principal or proxy-assumable
  role with equivalent catalog and data isolation.
- KV providers allocate a provider-private namespace and principal whose prefix cannot be
  selected or escaped by a workload.
- Providers unable to enforce database/namespace and catalog isolation cannot host guest
  data.

## Consequences

- Isolation is enforced by both verified Aseman identity and the database engine's role
  system, reducing reliance on perfect predicate injection.
- Guest applications can use multiple native tables/collections and indexes.
- Provisioning, role lifecycle, quotas, pool isolation, schema catalogs, backup, export,
  and deletion become explicit control-plane responsibilities.
- Large installations may have many logical databases and pools. Providers must declare
  capacity limits and use bounded/lazy pooling.
- Cross-creature sharing cannot be implemented with direct grants between creature roles;
  it remains an explicit Aseman capability-mediated operation.
- Native provider features may reduce portability. Compatibility reports and migration
  plans must expose that before activation or cutover.

## Migration

The legacy key space is first characterized and mapped to creature ownership. Migration
provisions each target creature database and role, installs its logical schemas, imports
that creature's capsules, verifies semantic reads and isolation, applies the final delta,
and switches the trusted proxy binding. No workload receives a migration credential.

## Rollback

Retain the old provider and immutable per-creature export/checkpoint through the rollback
window. Atomically restore the proxy binding to the prior provider generation. Newly
created target roles remain disabled and their databases retained for investigation until
explicit retirement approval.

## Rejected alternative

A universal `guest_capsules` table/collection keyed by `creature_id` was rejected because
it mixes tenants physically and prevents creature-controlled multi-table/collection
schemas. Row-level security may still protect control-plane tables, but it is not the
primary guest-data tenancy model.

## Review triggers

Review this decision if provider limits make per-creature databases operationally
infeasible, if a provider cannot support safe proxy role assumption, or if capsule export
cannot preserve a supported guest schema without weakening isolation.
