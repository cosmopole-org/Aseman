---
status: ACCEPTED
owner: migration/P0-04
source_of_truth: measured current-tree commands plus docs/generated/current-quality-baseline.json
last_verified_commit: 800df24076c7
verification: static generator check and rerun the recorded benchmark commands
---

# Phase 0 correctness and performance baseline

This report is the regression origin, not a performance claim. Measurements were
taken on the migration workspace on 2026-09-19 UTC. Host-dependent numbers are used
for order-of-magnitude regression detection; release thresholds require controlled
CI runners and the Phase 10 load/soak manifests.

## Correctness baseline

| Surface | Command | Result | Wall time | Peak RSS |
|---|---|---:|---:|---:|
| Node workspace | `cargo test --manifest-path node/Cargo.toml --workspace` | 401 passed, 0 failed | 10.20 s (warm build) | 132,744 KiB |
| Administrative CLI | `cargo test --manifest-path cmd/casparctl/Cargo.toml` | 33 passed, 0 failed | 13.43 s (cold dependency build) | 305,740 KiB |
| Characterization manifest | `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests/characterization -p 'test_*.py'` | 5 passed, 0 failed | recorded by P0 verification | not isolated |
| TypeScript client | `npm --prefix client-cli run typecheck` | not runnable: dependencies are not installed | n/a | n/a |

Known compiler warnings are one `unused_mut` in the node hashgraph store and one
unused `description` field in `casparctl`. They are baseline debt, not accepted
exceptions for new code.

## Static maintainability and allocation-risk baseline

The reproducible counts, sample locations, and twenty largest source files are in
`docs/generated/current-quality-baseline.{json,md}`. Run:

```bash
python3 scripts/generate_quality_baseline.py --check
```

The lexical `unwrap`, `expect`, panic, unsafe, sleep/spawn, JSON-value, environment
read, and lint-allow counts are ratchets. Phase 1 replaces them with scoped
architecture and lint checks; a migration may temporarily add a use only when its
work unit records the reason and deletion gate.

## Service latency, throughput, startup, and recovery

The current repository has no hermetic service benchmark: node startup requires an
owner key plus RocksDB/QuestDB paths, opens multiple listeners, and may join an
OpenRaft mesh; guest/VMM paths depend on external runtimes. Inventing local numbers
would hide those dependencies. The baseline therefore records the observable legacy
contract and the first measurable target for each dimension:

| Dimension | Current baseline | First mandatory measurable target |
|---|---|---|
| API latency/throughput | No repeatable harness; unsupported as a regression number. | Phase 1 in-process use-case benchmark; Phase 7 HTTP load manifest with p50/p95/p99 and error rate. |
| Startup | No readiness contract; startup can return early on missing key/storage and starts listeners in-process. | Phase 1 composition smoke test; Phase 9 readiness-to-ready duration on compact profile. |
| Steady memory/allocation | Unit-test peak RSS above; node includes `pprof` but has no stable workload fixture. | Phase 1 deterministic use-case allocation benchmark; Phase 5 VMM and Phase 7 gateway soak RSS slope. |
| Recovery | Unit tests cover transaction replay, VMM-listener restoration, and three-node replication, but no timed recovery SLO. | Each owning phase adds crash/restart checkpoint tests; Phase 10 records RTO/RPO and failover percentiles. |
| Storage migration | No capsule migration exists. | Phase 3 measures rows/bytes per second, semantic mismatch count, checkpoint recovery, and rollback time. |
| Guest isolation | Legacy host calls, no per-creature provider role. | Phases 3-4 measure pool cardinality, role-switch latency, connection reset failures, and prove zero cross-creature access. |

“No repeatable harness” is a measured capability gap and blocks performance claims,
not Phase 1 boundary work. No later phase may declare its performance/recovery gate
complete until its named target produces machine-readable evidence.

## Regression policy

- Correctness may not decrease: all supported characterized behavior stays green or
  is recorded as intentional removal with expiry and migration guidance.
- Static ratchet counts may not rise in new architecture crates; legacy counts are
  reduced as code moves and are checked against this snapshot.
- A measured p95 latency, throughput, peak-memory, startup, or recovery regression
  over 10% requires an explanation and explicit acceptance; security/correctness
  improvements must still remain within declared SLOs.
- Benchmarks record commit, profile/features, host identity, dataset, concurrency,
  warm-up, repetitions, distribution, peak RSS, and external service versions.
- Results from different host classes are not compared as if they were equivalent.
