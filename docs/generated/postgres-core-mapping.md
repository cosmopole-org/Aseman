---
status: GENERATED
owner: storage/postgres
source_of_truth: contracts/capsule/kinds, contracts/storage/postgres, and scripts/generate_postgres_core.py
last_verified_commit: 736b958e96b9
verification: python3 scripts/generate_postgres_core.py --check
---

# PostgreSQL core mapping

Every core kind has its own native table in `aseman_core`. `capsule_cbor` preserves
the signed canonical envelope while typed columns, foreign keys, partial unique
indexes, and checks enforce the accepted logical schema. No guest payload table exists.

| Kind | Table | Typed fields | Relationships | Unique indexes |
|---|---|---:|---:|---:|
| `core.user` | `aseman_core.users` | 4 | 0 | 3 |
| `core.creature` | `aseman_core.creatures` | 6 | 1 | 2 |
| `core.program` | `aseman_core.programs` | 4 | 1 | 0 |
| `core.store` | `aseman_core.stores` | 5 | 2 | 0 |
| `core.store_membership` | `aseman_core.store_memberships` | 4 | 3 | 1 |
| `core.access_level` | `aseman_core.access_levels` | 2 | 1 | 1 |
| `core.capability_grant` | `aseman_core.capability_grants` | 5 | 2 | 0 |
| `core.session` | `aseman_core.sessions` | 4 | 1 | 1 |
| `core.file` | `aseman_core.files` | 4 | 2 | 1 |
| `core.workload` | `aseman_core.workloads` | 6 | 2 | 1 |
| `core.workload_operation` | `aseman_core.workload_operations` | 5 | 1 | 1 |
| `core.node` | `aseman_core.nodes` | 4 | 0 | 1 |
| `core.federation_peer` | `aseman_core.federation_peers` | 4 | 0 | 1 |
| `core.node_key` | `aseman_core.node_keys` | 6 | 1 | 1 |
| `core.module_installation` | `aseman_core.module_installations` | 6 | 0 | 1 |
| `core.guest_database_binding` | `aseman_core.guest_database_bindings` | 6 | 1 | 3 |
| `core.guest_schema_definition` | `aseman_core.guest_schema_definitions` | 4 | 2 | 1 |
| `core.chain` | `aseman_core.chains` | 2 | 1 | 1 |
| `core.chain_shard` | `aseman_core.chain_shards` | 2 | 1 | 1 |
| `core.entity` | `aseman_core.entities` | 3 | 1 | 1 |
| `core.user_metadata` | `aseman_core.user_metadata_documents` | 3 | 1 | 1 |
| `core.creature_metadata` | `aseman_core.creature_metadata_documents` | 3 | 1 | 1 |
| `core.store_metadata` | `aseman_core.store_metadata_documents` | 3 | 1 | 1 |
| `core.program_metadata` | `aseman_core.program_metadata_documents` | 3 | 1 | 1 |
| `core.creature_type` | `aseman_core.creature_types` | 4 | 0 | 1 |
| `core.gateway_route` | `aseman_core.gateway_routes` | 3 | 2 | 1 |
| `core.program_alarm` | `aseman_core.program_alarms` | 3 | 2 | 1 |
| `core.vm_resource_store` | `aseman_core.vm_resource_stores` | 5 | 1 | 0 |
| `core.vm_resource_entity` | `aseman_core.vm_resource_entities` | 10 | 1 | 1 |
| `core.entity_config` | `aseman_core.entity_configs` | 3 | 1 | 1 |
| `core.entity_artifact` | `aseman_core.entity_artifacts` | 6 | 1 | 1 |
| `core.creature_secret` | `aseman_core.creature_secrets` | 4 | 1 | 1 |
| `core.secret_grant` | `aseman_core.secret_grants` | 2 | 2 | 1 |
| `core.bridge_grant` | `aseman_core.bridge_grants` | 6 | 0 | 1 |
| `core.bridge_topic` | `aseman_core.bridge_topics` | 2 | 0 | 1 |
| `core.legacy_identity` | `aseman_core.legacy_identities` | 4 | 0 | 2 |

The guest catalog tables contain only trusted bindings and schema definitions.
Creature-owned rows are stored later in separate provider-native databases/namespaces
under dedicated roles; they are never placed in a shared `guest_capsules` table.
