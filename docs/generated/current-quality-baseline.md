---
status: CURRENT
owner: migration/P0-04
source_of_truth: scripts/generate_quality_baseline.py
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_quality_baseline.py --check
---

# Current static quality baseline

Scanned 652 source files, 203311 physical lines, and 189072 nonblank lines.

## Ratchet counts

| Metric | Count |
|---|---:|
| `environment_reads` | 10 |
| `rust_allow_attributes` | 120 |
| `rust_expect_calls` | 600 |
| `rust_json_value_mentions` | 2291 |
| `rust_panic_macros` | 318 |
| `rust_sleep_calls` | 75 |
| `rust_spawn_calls` | 82 |
| `rust_unsafe_tokens` | 43 |
| `rust_unwrap_calls` | 3764 |

These lexical metrics include tests and comments. They establish a reproducible
ratchet; they do not assert that every occurrence is defective.

## Largest source files

| Path | Lines |
|---|---:|
| `crates/aseman-storage/src/client.rs` | 12478 |
| `modules/runtime/elpian/crates/elpian-vm/src/sdk/executor.rs` | 6591 |
| `crates/aseman-application/src/finance_actions.rs` | 3274 |
| `modules/storage/rocksdb/src/tests.rs` | 3039 |
| `modules/consensus/hashgraph/src/hashgraph/hashgraph.rs` | 2751 |
| `modules/runtime/elpian/crates/elpian-vm/src/sdk/stdlib/mod.rs` | 2468 |
| `apps/aseman-node/src/adapters/vmm/hostcall_entities.rs` | 2293 |
| `apps/aseman-node/src/api/public_http.rs` | 2258 |
| `crates/aseman-config/src/lib.rs` | 1951 |
| `modules/runtime/modal/src/controller.rs` | 1918 |
| `apps/aseman-node/src/adapters/vmm/host/vm_host_functions.rs` | 1875 |
| `modules/runtime/elpian/crates/elpian-vm/src/sdk/compiler.rs` | 1862 |
| `apps/aseman-client/index.ts` | 1796 |
| `apps/aseman-node/src/api/actions/creature.rs` | 1741 |
| `crates/aseman-contracts/src/capsule.rs` | 1673 |
| `crates/aseman-module-runtime/src/lib.rs` | 1581 |
| `modules/storage/postgres/src/lib.rs` | 1581 |
| `apps/aseman-node/src/api/actions/program.rs` | 1579 |
| `crates/aseman-ports/src/conformance.rs` | 1551 |
| `apps/asemanctl/src/cli/ops.rs` | 1550 |

## Limitations

- Lexical counts include tests/comments and are ratchets, not defect counts.
- Runtime performance and recovery measurements are recorded in docs/migration/baseline.md.
- Duplicate-code and cyclomatic-complexity tooling is introduced by Phase 1 xtask/CI.
