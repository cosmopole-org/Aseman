---
status: CURRENT
owner: storage/security migration
source_of_truth: docs/decisions/0001-creature-isolated-guest-databases.md
last_verified_commit: 800df24076c7
verification: rg -n "guest_capsules|one dynamic|single guest" plan/migration docs/decisions
---

# Plan change: creature-isolated guest databases

Requirement: R10, R11, R13, R14, R24  
Phase/work package: Phase 0 plan integrity / future P3-03 and P4-04  
Current owner/path: legacy RocksDB/VMM database host calls  
Target owner/path: guest-data proxy, capsule storage provider, creature database/role catalog  
Inputs/accepted ADRs: ADR 0001 accepted; A301/A306/A401 remain required  
State authority affected: Aseman owns creature-to-database/role/key bindings; the provider owns physical databases and role enforcement  
Contract/schema change: replaces shared `guest_capsules` tenancy with per-creature database/namespace and role mapping  
Migration/cutover: provision role/database, import per-creature capsules, verify isolation, atomically switch proxy binding  
Rollback: restore the prior provider-generation binding and disable the target role  
Security/threat impact: adds signature freshness/replay checks, role-confusion and pool-leakage threats, and provider catalog-isolation requirements  
Performance/index/backpressure impact: requires bounded/lazy per-database pools and declared provider tenant limits  
Tests and commands: plan consistency search now; Phase 3/4 provider, adversarial, migration, and property suites later  
Generated docs/inventories: A306/A401 and provider mappings must encode this decision  
Removal-ledger entries: legacy VM database host calls and any shared guest-table implementation  
Evidence and known limitations: algorithms and exact provider protocols remain blocked on A301/A306/A401; no implementation is authorized from this ADR alone.
