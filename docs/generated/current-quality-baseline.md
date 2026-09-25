---
status: CURRENT
owner: migration/P0-04
source_of_truth: scripts/generate_quality_baseline.py
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_quality_baseline.py --check
---

# Current static quality baseline

Scanned 604 source files, 172321 physical lines, and 160743 nonblank lines.

## Ratchet counts

| Metric | Count |
|---|---:|
| `environment_reads` | 6 |
| `rust_allow_attributes` | 36 |
| `rust_expect_calls` | 600 |
| `rust_json_value_mentions` | 1338 |
| `rust_panic_macros` | 323 |
| `rust_sleep_calls` | 78 |
| `rust_spawn_calls` | 78 |
| `rust_unsafe_tokens` | 43 |
| `rust_unwrap_calls` | 3229 |

These lexical metrics include tests and comments. They establish a reproducible
ratchet; they do not assert that every occurrence is defective.

## Largest source files

| Path | Lines |
|---|---:|
| `modules/runtime/elpian/crates/elpian-vm/src/sdk/executor.rs` | 6591 |
| `apps/aseman-node/src/shell/api/actions/creature/finance.rs` | 4152 |
| `modules/storage/rocksdb-legacy/src/tests.rs` | 3038 |
| `modules/consensus/hashgraph/src/hashgraph/hashgraph.rs` | 2759 |
| `modules/runtime/elpian/crates/elpian-vm/src/sdk/stdlib/mod.rs` | 2468 |
| `apps/aseman-node/src/drivers/vmm/hostcall_entities.rs` | 2311 |
| `modules/runtime/modal/src/controller.rs` | 1918 |
| `apps/asemanctl/src/cli/mod.rs` | 1901 |
| `apps/aseman-node/src/drivers/vmm/host/vm_host_functions.rs` | 1893 |
| `modules/runtime/elpian/crates/elpian-vm/src/sdk/compiler.rs` | 1862 |
| `apps/aseman-client/index.ts` | 1796 |
| `apps/aseman-node/src/shell/api/actions/program.rs` | 1681 |
| `crates/aseman-contracts/src/capsule.rs` | 1673 |
| `crates/aseman-module-runtime/src/lib.rs` | 1581 |
| `crates/aseman-ports/src/conformance.rs` | 1550 |
| `apps/aseman-node/src/shell/api/actions/creature.rs` | 1545 |
| `crates/aseman-config/src/lib.rs` | 1494 |
| `modules/vmm-http/src/server.rs` | 1439 |
| `apps/asemanctl/src/cli/ops.rs` | 1325 |
| `modules/runtime/elpify/crates/elpify-lang/src/compiler.rs` | 1324 |

## Limitations

- Lexical counts include tests/comments and are ratchets, not defect counts.
- Runtime performance and recovery measurements are recorded in docs/migration/baseline.md.
- Duplicate-code and cyclomatic-complexity tooling is introduced by Phase 1 xtask/CI.
