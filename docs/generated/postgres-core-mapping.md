---
status: GENERATED
owner: storage/postgres
source_of_truth: contracts/capsule/kinds, contracts/storage/postgres, and scripts/generate_postgres_core.py
last_verified_commit: 687a0ca24f8e
verification: python3 scripts/generate_postgres_core.py --check
---

# PostgreSQL core mapping

Every core kind has its own native table in `aseman_core`. By default (ADR 0034)
every field is a real column (a document field is a JSONB column) and the envelope is
rebuilt from the row; with capsule mode on, `capsule_cbor` also packs the signed
canonical envelope. Foreign keys, partial unique indexes, and checks enforce the
accepted logical schema in both layouts. No guest payload table exists.

| Kind | Table | Typed fields | Relationships | Unique indexes |
|---|---|---:|---:|---:|
| `core.user` | `aseman_core.users` | 4 | 0 | 3 |
| `core.creature` | `aseman_core.creatures` | 6 | 1 | 2 |
| `core.program` | `aseman_core.programs` | 4 | 1 | 0 |
| `core.store` | `aseman_core.stores` | 5 | 2 | 0 |
| `core.store_membership` | `aseman_core.store_memberships` | 4 | 3 | 1 |
| `core.access_level` | `aseman_core.access_levels` | 2 | 1 | 1 |
| `core.capability_grant` | `aseman_core.capability_grants` | 12 | 0 | 0 |
| `core.session` | `aseman_core.sessions` | 4 | 1 | 1 |
| `core.file` | `aseman_core.files` | 4 | 2 | 1 |
| `core.workload` | `aseman_core.workloads` | 6 | 2 | 1 |
| `core.workload_operation` | `aseman_core.workload_operations` | 5 | 1 | 1 |
| `core.node` | `aseman_core.nodes` | 4 | 0 | 1 |
| `core.federation_peer` | `aseman_core.federation_peers` | 4 | 0 | 1 |
| `core.identity_key` | `aseman_core.identity_keys` | 11 | 0 | 2 |
| `core.module_installation` | `aseman_core.module_installations` | 6 | 0 | 1 |
| `core.guest_database_binding` | `aseman_core.guest_database_bindings` | 6 | 1 | 3 |
| `core.guest_schema_definition` | `aseman_core.guest_schema_definitions` | 4 | 2 | 1 |
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
| `core.counter` | `aseman_core.counters` | 2 | 0 | 1 |
| `core.marker` | `aseman_core.markers` | 2 | 0 | 1 |
| `core.finance_account` | `aseman_core.finance_accounts` | 8 | 0 | 1 |
| `core.finance_hold` | `aseman_core.finance_holds` | 3 | 0 | 1 |
| `core.finance_pool` | `aseman_core.finance_pools` | 2 | 0 | 1 |
| `core.finance_pool_reservation` | `aseman_core.finance_pool_reservations` | 2 | 0 | 1 |
| `core.finance_live_debit` | `aseman_core.finance_live_debits` | 1 | 0 | 1 |
| `core.finance_project_budget` | `aseman_core.finance_project_budgets` | 1 | 0 | 1 |
| `core.finance_payout` | `aseman_core.finance_payouts` | 3 | 0 | 1 |
| `core.finance_journal` | `aseman_core.finance_journals` | 5 | 0 | 1 |
| `core.finance_journal_participant` | `aseman_core.finance_journal_participants` | 3 | 0 | 1 |
| `core.billing_catalog` | `aseman_core.billing_catalogs` | 1 | 0 | 1 |
| `core.billing_quote` | `aseman_core.billing_quotes` | 1 | 0 | 1 |
| `core.namespace_document` | `aseman_core.namespace_documents` | 1 | 0 | 1 |
| `core.replay_nonce` | `aseman_core.nonce_records` | 3 | 0 | 1 |
| `core.identity_challenge` | `aseman_core.auth_challenges` | 4 | 0 | 1 |
| `core.public_idempotency` | `aseman_core.public_idempotency_claims` | 7 | 0 | 1 |
| `core.session_token` | `aseman_core.session_tokens` | 2 | 0 | 1 |
| `core.vm_instance` | `aseman_core.vm_instances` | 7 | 0 | 1 |
| `core.vm_distribution` | `aseman_core.vm_distributions` | 2 | 0 | 1 |
| `core.vm_terminal` | `aseman_core.vm_terminals` | 4 | 0 | 1 |
| `core.token_lock` | `aseman_core.token_locks` | 3 | 0 | 1 |
| `core.proxy_correlation` | `aseman_core.proxy_correlations` | 2 | 0 | 1 |
| `core.guest_pair` | `aseman_core.guest_pairs` | 5 | 0 | 1 |
| `core.chain` | `aseman_core.chains` | 3 | 1 | 2 |
| `core.chain_shard` | `aseman_core.chain_shards` | 3 | 1 | 2 |

The guest catalog tables contain only trusted bindings and schema definitions.
Creature-owned rows are stored later in separate provider-native databases/namespaces
under dedicated roles; they are never placed in a shared `guest_capsules` table.
