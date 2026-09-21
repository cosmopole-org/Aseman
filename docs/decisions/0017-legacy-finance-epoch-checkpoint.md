---
status: DECISION
owner: finance/migration
source_of_truth: this ADR
last_verified_commit: f6be364d6761
verification: A307 generated finance mapping plus A308 legacy finance transforms, reconciliation, and tests
---

# ADR 0017: Legacy finance migrates as a reconciled, immutable epoch checkpoint

## Status

Accepted 2026-09-21. Required by A308 before any legacy finance record can be exported;
it constrains, but does not implement, P8 ledger conversion.

## Context

Legacy Caspar keeps a complete financial subsystem in `creature/finance.rs`, stored as
`json::` documents and `link::` counters and markers. The subsystem includes holds,
pools, pool reservations, live debits, payouts, project budgets, a journal whose entries
already name debit/credit accounts, pricing catalogs, quotes, market and node
registries, locked tokens, and per-VM billing bindings. A legacy administrative action
(`/creatures/reconcileFinancialSystem`) defines which values are authorities and which
are derived counters.

Plan 07 assigns double-entry settlement, pricing, and consensus to P8 and requires every
provider change to happen at a finalized epoch with checkpoint, reconciliation, and
rollback data. Phase 3 must still migrate every persistent datum, and it must not invent
ledger semantics.

## Decision

Phase 3 exports legacy finance as one immutable **legacy finance epoch**:

- Each authoritative legacy finance record becomes a `finance.legacy_record` capsule in
  the finance storage class. It is serializable, append-only, and permanently
  retained. Its typed fields are `record_family` (closed set), `legacy_key`,
  `currency`, `scale`, `entry_count`, and `content_digest`; its structured `document`
  field follows ADR 0016 and has no native column. `(record_family, legacy_key)` is
  unique.
- The record families are:
  - JSON records: `hold`, `pool`, `pool_reservation`, `live_debit`, `payout`,
    `journal_entry`, `project_budget`, `billing_catalog`, `billing_quote`,
    `billing_namespace`, `market_namespace`, `vm_billing`, and `token_lock`.
  - Authoritative counters: `debt_counter` and `withdrawable_counter`.
  - Idempotency markers: `hold_request`, `hold_run`, `hold_settlement`,
    `hold_release`, `payout_request`, `payout_resolution`, `pool_open`,
    `pool_refresh`, `pool_close`, `pool_settlement`, `pool_debit`, and
    `payment_adjustment`.
- Markers are migrated because dropping them would let a replayed request or settlement
  apply twice after cutover.
- Derived legacy counters (`FinanceHeld`, `FinancePayoutHeld`, `FinanceSpent`,
  `FinanceEarned`) and the per-party listing links (`FinanceHoldByPayer`,
  `FinanceJournalByUser`, `FinancePayoutByUser`, `FinancePoolByUser`) are rebuilt and
  compared, never migrated.
- Amounts stay integer minor units in the currency and scale that the migration runner
  supplies explicitly. No legacy amount is converted, rounded, or re-priced.
- Records are not tenant-scoped rows: several creatures take part in one hold,
  settlement, or journal entry. Owner scope is global. P8 derives per-creature ledger
  accounts and wallet opening balances from the checkpoint.

## Reconciliation gate

The export runs every invariant of the legacy reconciliation action and fails closed on
any issue, where legacy only reports it:

- Holds must have a valid status and consistent max, remaining, actual, and refunded
  amounts, and settlement lines must sum to the actual amount.
- Pools must satisfy `maxAmount == remaining + reserved + spent + refunded`, and each
  pool's stored `reserved` must equal its open reservations.
- Settled reservations and live debits must have credits that sum to the charged amount.
- Stored held, payout-held, spent, and earned counters must equal their reconstructions.
- Every withdrawable amount must be within its creature balance, and total withdrawable
  must not exceed total earnings.
- A project budget's reserved amount must equal its expected value, and its spent amount
  must not be below its expected value.

A listing link must name an existing record of its family whose party matches. Any
other malformed, unknown, or unreviewed finance key fails closed.

## P8 boundary

P8 converts the checkpoint into double-entry ledger entries and wallet opening balances,
and it keeps each legacy record's digest as the conversion's source evidence. Until P8
passes its gate, the legacy finance subsystem stays authoritative and this checkpoint is
read-only comparison and rollback data.

## Rejected alternatives

- Converting legacy records to double-entry in Phase 3: this would invent P8 settlement
  semantics before its ADR and conformance work exist.
- One table per legacy finance family: this fixes an interim format into the permanent
  schema, and P8 replaces it anyway.
- Migrating derived counters as authorities: they would shadow drift that the legacy
  reconciliation defines as an error.
