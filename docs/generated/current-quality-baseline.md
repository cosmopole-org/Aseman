---
status: CURRENT
owner: migration/P0-04
source_of_truth: scripts/generate_quality_baseline.py
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_quality_baseline.py --check
---

# Current static quality baseline

Scanned 398 source files, 110200 physical lines, and 102264 nonblank lines.

## Ratchet counts

| Metric | Count |
|---|---:|
| `environment_reads` | 3 |
| `rust_allow_attributes` | 34 |
| `rust_expect_calls` | 325 |
| `rust_json_value_mentions` | 1105 |
| `rust_panic_macros` | 300 |
| `rust_sleep_calls` | 58 |
| `rust_spawn_calls` | 72 |
| `rust_unsafe_tokens` | 40 |
| `rust_unwrap_calls` | 1752 |

These lexical metrics include tests and comments. They establish a reproducible
ratchet; they do not assert that every occurrence is defective.

## Largest source files

| Path | Lines |
|---|---:|
| `vms/elpian/crates/elpian-vm/src/sdk/executor.rs` | 6490 |
| `node/src/shell/api/actions/creature/finance.rs` | 4119 |
| `node/src/drivers/network/chain/hashgraph/hashgraph.rs` | 2761 |
| `node/src/drivers/vmm/hostcall_entities.rs` | 2369 |
| `vms/elpian/crates/elpian-vm/src/sdk/stdlib/mod.rs` | 2096 |
| `vms/modal/src/controller.rs` | 1918 |
| `cmd/casparctl/src/main.rs` | 1890 |
| `node/src/drivers/vmm/host/vm_host_functions.rs` | 1860 |
| `node/src/shell/api/actions/program.rs` | 1857 |
| `client-cli/index.ts` | 1796 |
| `vms/elpian/crates/elpian-vm/src/sdk/compiler.rs` | 1760 |
| `crates/aseman-contracts/src/capsule.rs` | 1673 |
| `node/src/shell/api/actions/creature.rs` | 1599 |
| `run-nodes.sh` | 1594 |
| `crates/aseman-module-runtime/src/lib.rs` | 1581 |
| `vms/elpify/crates/elpify-lang/src/compiler.rs` | 1324 |
| `vms/docker/src/controller.rs` | 1260 |
| `node/src/core/core_orchestrator.rs` | 1222 |
| `node/src/core/actor/model/trx.rs` | 1210 |
| `node/src/drivers/vmm/driver.rs` | 1190 |

## Limitations

- Lexical counts include tests/comments and are ratchets, not defect counts.
- Runtime performance and recovery measurements are recorded in docs/migration/baseline.md.
- Duplicate-code and cyclomatic-complexity tooling is introduced by Phase 1 xtask/CI.
