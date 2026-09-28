---
status: CURRENT
owner: migration/P0-04
source_of_truth: scripts/generate_quality_baseline.py
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_quality_baseline.py --check
---

# Current static quality baseline

Scanned 619 source files, 189865 physical lines, and 177313 nonblank lines.

## Ratchet counts

| Metric | Count |
|---|---:|
| `environment_reads` | 10 |
| `rust_allow_attributes` | 49 |
| `rust_expect_calls` | 603 |
| `rust_json_value_mentions` | 1693 |
| `rust_panic_macros` | 317 |
| `rust_sleep_calls` | 77 |
| `rust_spawn_calls` | 81 |
| `rust_unsafe_tokens` | 43 |
| `rust_unwrap_calls` | 3564 |

These lexical metrics include tests and comments. They establish a reproducible
ratchet; they do not assert that every occurrence is defective.

## Largest source files

| Path | Lines |
|---|---:|
| `modules/runtime/elpian/crates/elpian-vm/src/sdk/executor.rs` | 6591 |
| `apps/aseman-node/src/api/actions/creature/finance.rs` | 4144 |
| `crates/aseman-application/src/finance_actions.rs` | 3274 |
| `modules/storage/rocksdb/src/tests.rs` | 3039 |
| `modules/consensus/hashgraph/src/hashgraph/hashgraph.rs` | 2749 |
| `modules/runtime/elpian/crates/elpian-vm/src/sdk/stdlib/mod.rs` | 2468 |
| `apps/aseman-node/src/api/actions/program.rs` | 2439 |
| `apps/aseman-node/src/adapters/vmm/hostcall_entities.rs` | 2311 |
| `apps/aseman-node/src/api/public_http.rs` | 2281 |
| `modules/runtime/modal/src/controller.rs` | 1918 |
| `crates/aseman-config/src/lib.rs` | 1905 |
| `apps/aseman-node/src/adapters/vmm/host/vm_host_functions.rs` | 1874 |
| `modules/runtime/elpian/crates/elpian-vm/src/sdk/compiler.rs` | 1862 |
| `apps/aseman-client/index.ts` | 1796 |
| `apps/aseman-node/src/api/actions/creature.rs` | 1791 |
| `crates/aseman-contracts/src/capsule.rs` | 1673 |
| `crates/aseman-module-runtime/src/lib.rs` | 1581 |
| `apps/asemanctl/src/cli/ops.rs` | 1550 |
| `crates/aseman-ports/src/conformance.rs` | 1550 |
| `modules/network/http/src/lib.rs` | 1541 |

## Limitations

- Lexical counts include tests/comments and are ratchets, not defect counts.
- Runtime performance and recovery measurements are recorded in docs/migration/baseline.md.
- Duplicate-code and cyclomatic-complexity tooling is introduced by Phase 1 xtask/CI.
