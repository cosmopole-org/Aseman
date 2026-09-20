# Adaptive Capsule Storage

## Universal rule

Every persistent datum uses the capsule protocol, including core state, guest data, telemetry, audit, finance, outbox records, realtime delivery state, and module-owned state. Storage classes are routing and policy labels; none bypasses the capsule layer.

```rust
pub struct CapsuleEnvelope {
    pub id: CapsuleId,
    pub kind: CapsuleKind,
    pub storage_class: StorageClass,
    pub owner_scope: OwnerScope,
    pub schema_version: u32,
    pub revision: u64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub integrity_hash: Digest,
    pub body: CapsuleBody,
}
```

The envelope is the portable exchange, backup, migration, integrity, and replication form. It is not a mandate to store every entity as one JSON/blob row.

## Storage classes

```text
Core
GuestData
Telemetry
Audit
Finance
Outbox
Realtime
Module(<module-name>)
```

An administrator may bind each class or capsule kind to a compatible provider. A compact installation may route all classes to one PostgreSQL instance.

## Core entities

Core state stays strongly typed. Application ports such as `CreatureRepository`, `WorkloadRepository`, and `PolicyRepository` translate domain operations into capsule commands and semantic queries. The provider owns physical mapping.

For SQL databases, every core entity type maps to its own native table. Expected tables include:

```text
users
creatures
programs
stores
store_memberships
access_levels
capability_grants
workloads
workload_operations
nodes
federation_peers
node_keys
wallets
ledger_entries
pricing_policies
usage_records
module_installations
guest_database_bindings
guest_schema_definitions
```

Foreign keys, uniqueness, checks, typed columns, transactions, and indexes must be used natively. JSON/JSONB is reserved for genuinely extensible fields, not for hiding all core entities in a universal table.

Document providers use a separate collection per core kind. Graph providers use typed vertices and relationships. KV providers use provider-private key spaces and secondary indexes.

## Guest data

Guest data does not use one shared table or collection. Each creature receives one provider-native logical database or equivalently isolated namespace and a dedicated provider role/principal restricted to it. All programs and workloads owned by the same creature resolve to that database and role. Inside it, the creature may define multiple tables, collections, relationships, and indexes subject to provider capabilities, quotas, reserved metadata, schema validation, and administrator policy.

For PostgreSQL, the default mapping is a dedicated database and a `NOLOGIN` role assumed only by the Aseman guest-data proxy. Public database/schema privileges are revoked; the creature role receives constrained DDL/DML rights only inside its database. The control database stores typed mappings such as:

```text
guest_database_bindings(
  creature_id, provider_id, database_name, role_name,
  generation, schema_catalog_revision, status
)
```

Document providers allocate a dedicated database/namespace and proxy-assumable principal. KV providers allocate a provider-private namespace and role whose prefix cannot be selected or escaped by a workload. A provider that cannot prevent cross-database access and catalog inference cannot advertise guest-data support.

### Signed proxy authentication

A workload opens or uses a guest-data session by signing a canonical, audience-bound request or challenge with its registered workload key. The authenticated material includes the operation/request digest, nonce, issue/expiry time, audience, and key epoch. The proxy verifies signature, freshness, revocation, and replay protection, then resolves workload -> program -> creature from authoritative Aseman state.

The proxy selects `(provider, database/namespace, role)` exclusively from the trusted creature binding. A workload cannot nominate or override any of them. The proxy then assumes the provider role for the operation or transaction. Workloads receive no database password, provider service credential, or administration connection.

Connection pools are partitioned by provider and database, bounded and lazy, and never keyed by caller input. Role assumption is transaction-scoped; every checkout and return resets and verifies authorization state. Tests must prove that cancellations, errors, retries, and pool reuse cannot leak a previous creature role or database.

### Guest schemas and capsules

A creature-created table or collection definition is a versioned guest capsule definition. Each record/document is a logical capsule, but providers map it natively into that creature's chosen table/collection instead of a universal physical `guest_capsules` structure. Capsule identity, revision, ownership, timestamps, integrity, and tombstone metadata live in protected native fields or a provider-private sidecar and are reconstructed in canonical exports.

Schema operations pass through the authenticated proxy and may create, alter, index, or remove structures only in the resolved creature database. Reserved fields/names, destructive changes, retention, quotas, and portability checks remain policy controlled. Provider-specific schema features must be declared; unsupported migration targets fail with a compatibility report rather than silently dropping semantics.

Defense in depth:

- Public-key workload authentication with expiry, audience binding, nonces, key epochs, and revocation.
- Server-side workload -> program -> creature -> database/role resolution.
- Provider-native database/namespace and role isolation, including catalog isolation.
- Transaction-scoped role assumption and pool reset/contamination tests.
- Schema/DDL allowlists, reserved metadata, quotas, and bounded queries.
- Signed, creature/database-bound cursors, caches, events, and export checkpoints.
- Optional per-creature encryption keys.
- Audit capsules for authentication, schema changes, role changes, and data operations.

Cross-creature sharing requires an explicit administrator-issued capability and a dedicated mediated operation. Roles are never granted directly across creature databases. A guessed database name, role, key, ID, query, schema, or cursor must not reveal whether another creature's data exists.

## Capsule definitions

Each kind has a versioned definition containing fields, types, required values, constraints, relationships, indexes, searchable/sortable fields, ownership, retention, encryption, and migration functions. Providers validate definitions and generate native physical schema plans.

```rust
pub trait CapsuleMapper {
    fn validate_definition(&self, definition: &CapsuleDefinition)
        -> Result<CompatibilityReport>;
    fn plan_physical_schema(&self, definition: &CapsuleDefinition)
        -> Result<SchemaPlan>;
    fn encode_mutation(&self, capsule: &CapsuleEnvelope)
        -> Result<ProviderMutation>;
    fn decode_entity(&self, kind: &CapsuleKind, value: ProviderValue)
        -> Result<CapsuleEnvelope>;
}
```

## Queries

A bounded typed query AST supports declared fields, equality/range comparisons, boolean composition, relationship traversal, projection, sorting, aggregates, and cursor pagination. Providers translate it to native queries. Guest schema operations use a provider-neutral validated DDL model. Raw SQL, Mongo expressions, RocksDB prefixes, provider administration commands, and unrestricted user expressions remain forbidden at the Aseman/VM boundary; database ownership means control through the restricted proxy contract, not an escape from capsule, policy, or portability rules.

## Provider capabilities

Providers advertise at least:

```text
transactions.single_capsule
transactions.multi_capsule
relationships.foreign_keys
queries.range
queries.full_text
queries.relationship_traversal
indexes.unique
events.change_stream
consistency.linearizable
consistency.eventual
append_only.verifiable
guest_database.isolated_roles
guest_database.catalog_isolation
guest_database.schema_management
guest_database.safe_role_assumption
```

Aseman defines mandatory guarantees per storage class/kind. Activation fails if the provider cannot supply or safely emulate them. Financial, authorization, idempotency, and outbox records must never silently fall back to weaker consistency.

## Telemetry, audit, finance, and delivery state

- Telemetry capsule kinds map to separate native time-series tables/measurements/collections, such as workload usage and node health. SQL providers may partition these tables by time.
- Audit capsules are append-only and contain actor, target, policy decision/version, trace ID, timestamps, and integrity chaining/checkpoints.
- Wallets, ledger entries, pricing policies, and usage records map to separate typed finance tables/collections.
- Outbox, realtime subscription, and durable delivery capsules map to explicit native structures and retain capsule portability.

## Provider migration

The portable migration unit is a canonical `CapsuleEnvelope` stream, not raw rows, documents, or keys.

1. Validate target capabilities and logical schemas.
2. Provision target creature databases/namespaces and disabled dedicated roles; generate and review physical schemas.
3. Export capsules, guest schema definitions, relationships, revisions, tombstones, hashes, and creature database bindings.
4. Import each creature through the target provider's native mapper without issuing workload credentials.
5. Verify counts, hashes, ownership, relationships, semantic reads, role isolation, catalog isolation, and schema behavior.
6. Enable change capture or dual writes through the trusted proxy binding.
7. Apply and verify the final delta.
8. Atomically switch the provider/database/role binding generation and enable the target role.
9. Observe a rollback window while retaining the old provider and disabled target/old role state needed for reversal.
10. Retire the old provider databases and roles only after final approval.

This permits a RocksDB entity represented by several keys to become one normalized SQL entity across its own table and relationships, without carrying the RocksDB layout into PostgreSQL.

## Storage CLI

```text
asemanctl storage providers
asemanctl storage capabilities <provider>
asemanctl storage schema list
asemanctl storage schema inspect <kind>
asemanctl storage guest-database status <creature>
asemanctl storage guest-database doctor <creature>
asemanctl storage guest-database reconcile <creature>
asemanctl storage migration plan
asemanctl storage migration start
asemanctl storage migration status
asemanctl storage migration verify
asemanctl storage migration cutover
asemanctl storage migration rollback
```
