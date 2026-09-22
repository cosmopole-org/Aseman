---
status: CURRENT
owner: migration/P0-04
source_of_truth: scripts/generate_quality_baseline.py
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_quality_baseline.py --check
---

# Current static quality baseline

Scanned 504 source files, 150486 physical lines, and 140408 nonblank lines.

## Ratchet counts

| Metric | Count |
|---|---:|
| `environment_reads` | 3 |
| `rust_allow_attributes` | 35 |
| `rust_expect_calls` | 328 |
| `rust_json_value_mentions` | 1295 |
| `rust_panic_macros` | 312 |
| `rust_sleep_calls` | 62 |
| `rust_spawn_calls` | 73 |
| `rust_unsafe_tokens` | 43 |
| `rust_unwrap_calls` | 2744 |

These lexical metrics include tests and comments. They establish a reproducible
ratchet; they do not assert that every occurrence is defective.

## Largest source files

| Path | Lines |
|---|---:|
| `vms/elpian/crates/elpian-vm/src/sdk/executor.rs` | 6591 |
| `node/src/shell/api/actions/creature/finance.rs` | 4138 |
| `modules/storage-legacy/src/tests.rs` | 3038 |
| `node/src/drivers/network/chain/hashgraph/hashgraph.rs` | 2761 |
| `vms/elpian/crates/elpian-vm/src/sdk/stdlib/mod.rs` | 2468 |
| `node/src/drivers/vmm/hostcall_entities.rs` | 2341 |
| `node/src/drivers/vmm/host/vm_host_functions.rs` | 1962 |
| `vms/modal/src/controller.rs` | 1918 |
| `cmd/casparctl/src/main.rs` | 1890 |
| `vms/elpian/crates/elpian-vm/src/sdk/compiler.rs` | 1862 |
| `node/src/shell/api/actions/program.rs` | 1833 |
| `client-cli/index.ts` | 1796 |
| `crates/aseman-contracts/src/capsule.rs` | 1673 |
| `run-nodes.sh` | 1594 |
| `crates/aseman-module-runtime/src/lib.rs` | 1581 |
| `node/src/shell/api/actions/creature.rs` | 1545 |
| `crates/aseman-ports/src/conformance.rs` | 1479 |
| `modules/vmm-http/src/server.rs` | 1439 |
| `vms/elpify/crates/elpify-lang/src/compiler.rs` | 1324 |
| `modules/storage-postgres/src/lib.rs` | 1297 |

## Limitations

- Lexical counts include tests/comments and are ratchets, not defect counts.
- Runtime performance and recovery measurements are recorded in docs/migration/baseline.md.
- Duplicate-code and cyclomatic-complexity tooling is introduced by Phase 1 xtask/CI.
