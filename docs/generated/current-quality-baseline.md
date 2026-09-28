---
status: CURRENT
owner: migration/P0-04
source_of_truth: scripts/generate_quality_baseline.py
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_quality_baseline.py --check
---

# Current static quality baseline

Scanned 616 source files, 189448 physical lines, and 176855 nonblank lines.

## Ratchet counts

| Metric | Count |
|---|---:|
| `environment_reads` | 8 |
| `rust_allow_attributes` | 54 |
| `rust_expect_calls` | 610 |
| `rust_json_value_mentions` | 1743 |
| `rust_panic_macros` | 322 |
| `rust_sleep_calls` | 79 |
| `rust_spawn_calls` | 80 |
| `rust_unsafe_tokens` | 43 |
| `rust_unwrap_calls` | 3474 |

These lexical metrics include tests and comments. They establish a reproducible
ratchet; they do not assert that every occurrence is defective.

## Largest source files

| Path | Lines |
|---|---:|
| `modules/runtime/elpian/crates/elpian-vm/src/sdk/executor.rs` | 6591 |
| `apps/aseman-node/src/api/actions/creature/finance.rs` | 4144 |
| `crates/aseman-application/src/finance_actions.rs` | 3274 |
| `modules/storage/rocksdb-legacy/src/tests.rs` | 3039 |
| `modules/consensus/hashgraph/src/hashgraph/hashgraph.rs` | 2749 |
| `apps/aseman-node/src/api/actions/program.rs` | 2479 |
| `modules/runtime/elpian/crates/elpian-vm/src/sdk/stdlib/mod.rs` | 2468 |
| `apps/aseman-node/src/adapters/vmm/hostcall_entities.rs` | 2283 |
| `apps/aseman-node/src/api/public_http.rs` | 2281 |
| `apps/asemanctl/src/cli/mod.rs` | 2006 |
| `modules/runtime/modal/src/controller.rs` | 1918 |
| `apps/aseman-node/src/api/actions/creature.rs` | 1906 |
| `apps/aseman-node/src/adapters/vmm/host/vm_host_functions.rs` | 1879 |
| `modules/runtime/elpian/crates/elpian-vm/src/sdk/compiler.rs` | 1862 |
| `apps/aseman-client/index.ts` | 1796 |
| `crates/aseman-config/src/lib.rs` | 1793 |
| `crates/aseman-contracts/src/capsule.rs` | 1673 |
| `crates/aseman-module-runtime/src/lib.rs` | 1581 |
| `crates/aseman-ports/src/conformance.rs` | 1550 |
| `modules/network/http/src/lib.rs` | 1541 |

## Limitations

- Lexical counts include tests/comments and are ratchets, not defect counts.
- Runtime performance and recovery measurements are recorded in docs/migration/baseline.md.
- Duplicate-code and cyclomatic-complexity tooling is introduced by Phase 1 xtask/CI.
