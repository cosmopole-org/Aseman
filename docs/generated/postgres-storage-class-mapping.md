---
status: GENERATED
owner: storage/postgres
source_of_truth: contracts/capsule/kinds and scripts/generate_postgres_storage_classes.py
last_verified_commit: 27806f28b843
verification: python3 scripts/generate_postgres_storage_classes.py --check
---

# PostgreSQL non-core storage-class mapping

Each kind has a native typed table and preserves its canonical capsule envelope.
Append-only, consistency, retention, and query-index policies are explicit; no
universal payload table or JSONB entity bucket is used.

| Kind | Native table | Consistency | Mutation | Retention |
|---|---|---|---|---|
| `telemetry.workload_sample` | `aseman_telemetry.workload_samples` | `eventual` | `eventual_observation` | `telemetry_bounded` |
| `telemetry.node_health` | `aseman_telemetry.node_health_samples` | `eventual` | `eventual_observation` | `telemetry_bounded` |
| `telemetry.build_log` | `aseman_telemetry.build_logs` | `eventual` | `eventual_observation` | `telemetry_bounded` |
| `audit.event` | `aseman_audit.audit_events` | `append_linearizable` | `append_only` | `audit_permanent` |
| `finance.wallet` | `aseman_finance.wallets` | `serializable` | `mutable_cas` | `financial_permanent` |
| `finance.ledger_entry` | `aseman_finance.ledger_entries` | `serializable` | `append_only` | `financial_permanent` |
| `finance.pricing_policy` | `aseman_finance.pricing_policies` | `serializable` | `append_only` | `financial_permanent` |
| `finance.usage_record` | `aseman_finance.usage_records` | `serializable` | `append_only` | `financial_permanent` |
| `outbox.message` | `aseman_outbox.messages` | `serializable` | `mutable_cas` | `outbox_until_delivered` |
| `realtime.event` | `aseman_realtime.events` | `append_linearizable` | `append_only` | `realtime_by_class` |
| `realtime.subscription` | `aseman_realtime.subscriptions` | `serializable` | `mutable_cas` | `realtime_subscription` |
| `realtime.consumer_offset` | `aseman_realtime.consumer_offsets` | `serializable` | `mutable_cas` | `realtime_subscription` |
| `realtime.dead_letter` | `aseman_realtime.dead_letters` | `serializable` | `mutable_cas` | `realtime_by_class` |
| `finance.legacy_record` | `aseman_finance.legacy_finance_records` | `serializable` | `append_only` | `financial_permanent` |

Detailed finance, realtime, and retention behavior remains governed by their
later application/provider contracts; this mapping cannot weaken A307 guarantees.
