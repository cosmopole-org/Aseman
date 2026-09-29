---
status: DECISION
owner: architecture/composition
source_of_truth: this ADR; completes RL-001's "constructor-inject typed config"
last_verified_commit: pending
verification: cargo xtask fast; cargo xtask full (live PostgreSQL); clippy -D warnings on every workspace crate
---

# ADR 0037: Configuration is parsed once and passed in; shared adapter plumbing has one owner

## Status

Accepted 2026-09-29.

## Context

Three process-wide configuration locators remained after RL-001:

- `aseman_config::install_legacy_adapter_snapshot` / `legacy_adapter_snapshot` /
  `consensus_root_node` (`ACTIVE_CONFIG`), read by Hashgraph, the RocksDB provider,
  the storage migration, and node adapters;
- `aseman_config::runtime_config()`, read by the legacy runtime plugins, which fell back
  to re-reading the process environment and turned a parse error into defaults;
- `aseman_config::cli_config()` (`ACTIVE_CLI_CONFIG`) in `asemanctl`.

Because several node paths still read the process environment, the node copied `.env`
into it with `unsafe { std::env::set_var }`. The locators hid real defects: a process
that never installed the snapshot silently used defaults (`asemanctl storage migrate`
ignored the configured QuestDB port), a misconfigured public-HTTP or federation listener
silently did not start, and a test that set a retired variable name tested nothing.

Separately, adapter plumbing was copied: PostgreSQL pool construction and driver-error
mapping in five adapters, "write a temporary file and rename it" in seven places (with
inconsistent durability and a window where secret files were world-readable), and the
per-VM sandbox directory guard in two runtime plugins.

## Decision

1. **Parse once, pass in.** Each executable parses its configuration once at its
   composition root (`AsemanConfig`, `RuntimeConfig`, `CliConfig`) and passes typed
   values to what it constructs. No library reads configuration from a global, and none
   reads the process environment; `aseman-config` has no process-wide state.
   - The node's listener configurations (`public_http`, `federation_listener`,
     `federation_outbound`) and consensus properties are `AsemanConfig` fields. A
     listener is `None` when none of its required keys is set; a partly set or invalid
     one is a startup error.
   - Hashgraph takes its frame limits and key-mirror directory in its `Config`; it no
     longer depends on `aseman-config`. The node derives them in `ChainSettings`.
   - The RocksDB provider takes `RocksDbTuning` in `ProviderSettings`.
   - Runtime plugins are registered with `register(&RuntimeConfig)`; each keeps its own
     settings (`WasmSettings`, `FireSettings`, `DockerSettings`, `ModalSettings`), and
     the native backend parses `RuntimeConfig` once and fails on an invalid value.
2. **Fail closed at startup.** Configured TLS that cannot be loaded stops the node
   instead of falling back to plaintext.
3. **One owner for shared plumbing.**
   - `aseman-postgres`: the pool type, pool construction, connection checkout, and the
     one `postgres::Error -> PortError` mapping for every adapter that owns a
     PostgreSQL schema. `PortError::failed` constructs an adapter failure.
   - `aseman-fs`: `write_atomic` and the no-clobber `create_atomic`, which create the
     temporary file with its final permissions, sync it, rename it, and sync the
     directory.
   - `aseman_vm_sdk::sandbox::VmSandbox`: the per-VM sandbox directories and their
     containment guard, for every runtime that keeps per-VM files.

## Consequences

- The `unsafe` environment mutation is gone; `.env` reaches configuration only through
  `AsemanConfig::from_process_with_dotenv`.
- `aseman_config::{runtime_config, cli_config, install_cli_process_config,
  install_legacy_adapter_snapshot, legacy_adapter_snapshot, consensus_root_node,
  consensus_env_properties, process_state_home}` are removed, with the dead
  `CliConfig` and `LegacyAdapterConfig` fields they served.
- The node master key is only generated when none exists and is never replaced; a key
  that cannot be read is an error.
- New plugins scaffolded by `asemanctl vms new` and the generated `aseman-vm-plugins`
  crate use `register(&RuntimeConfig)`.
