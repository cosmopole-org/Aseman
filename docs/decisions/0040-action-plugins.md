---
status: DECISION
owner: architecture
source_of_truth: this ADR; crates/aseman-action-sdk; modules/actions/
last_verified_commit: pending
verification: cargo test -p aseman-node --lib actions::tests; cargo xtask fast; cargo xtask full
---

# ADR 0040: The node's actions are plugins, loaded at startup and connected to the router

## Status

Accepted 2026-09-30.

## Context

ADR 0039 left every operation declared once in the node's operation table
(`apps/aseman-node/src/actions/mod.rs`): a `const OPERATIONS` slice of
(path, origin, handler) tuples that `Router::new` binds to the A402 registry.
The node *is* its action set: adding, removing, or replacing an operation is a
change to the node's source, there is no way to ship an action independently,
and the node's crate carries the full action surface next to its transports and
composition.

The module platform (ADR 0015) already established an in-process, statically
linked plugin mechanism for VM runtimes: `aseman-vm-sdk` defines the `VmPlugin`
trait, a global registry, and a `VmHost` bridge; each runtime under
`modules/runtime/` exposes a `register()` entry point; a generated aggregation
crate (`vm-plugins`) calls each `register()` at startup; and the node resolves
every runtime through the registry, never naming a VM type. Dynamic loading is
blocked by the workspace's `unsafe_code = deny` (Rust has no stable plugin ABI),
so the plugin boundary is a trait plus a global registry inside one process.

The node's actions need the same boundary. Nothing in the node should have to
name an operation; the operation set should be whatever plugins register, and
plugins should reach the node only through a published interface.

## Decision

1. **An action plugin SDK.** New crate `crates/aseman-action-sdk`, mirroring
   `aseman-vm-sdk`, owns:
   - `ActionPlugin` — the trait an action plugin implements: a key and name,
     the operations it contributes (`ActionOperationSpec`: path and origin),
     and `run(ctx, path, input)`.
   - `ActionContext` — the host bridge a handler runs against: the node facade,
     the operation's transaction, and the caller. The node implements it once;
     plugins never see node internals.
   - `ActionNode` / `ActionTools` and the narrow service interfaces
     (`ActionStorage`, `ActionSecurity`, `ActionSignaler`, `ActionWorkloads`,
     `ActionNetwork`, `ActionVmm`) the node publishes through the facade.
   - `registry` — the global `register_plugin` / `plugins` registry.
   - The state port adapters, wire models, and wire shapes the actions use
     (`state/*`, `wire/*`), moved out of `apps/aseman-node/src/state/` and
     `apps/aseman-node/src/actions/wire/`, plus the shared helpers the handlers
     call (`secure_unique_string`, `async_once`, `secret_crypto`,
     `SystemClock`, `StorageRootBlobStore`, the proxy deploy config). The node
     re-exports them so its own code is unchanged.
2. **Every operation is a plugin.** The families currently in `actions/` become
   plugin crates under `modules/actions/`: `diagnostics`, `creatures`,
   `secrets`, `files`, `finance`, `stores`, `gateway`, `programs`, `workloads`.
   Each implements `ActionPlugin`, declares its operations, and exposes
   `register()`.
3. **No default wired action.** The `OPERATIONS` table is deleted.
   `Router::new` builds the operation table from whatever the registry holds:
   each plugin's operation must be a registered A402 shell surface (its action
   and packet guard still come from the registry, ADR 0039), a registered
   surface with no plugin is an error, and a path claimed by two plugins is an
   error. `actions/tests.rs` keeps pinning that every registered surface has
   exactly one operation.
4. **Plugins load at the runtime phase.** In `NodeApp::start`, after the node's
   components load, an aggregation crate (`aseman-action-plugins`, mirroring
   `vm-plugins`) calls every plugin's `register()`; the router is then built
   from the registry, automatically connecting the plugins to the router.
   Startup services that are not operations (`install_creature_types`,
   `start_workload_services`) remain node-side composition, not plugins.
5. **The boundary holds.** `modules/actions/*` and `crates/aseman-action-sdk`
   depend on domain, application, ports, contracts, storage, and the SDK; they
   never depend on `aseman-node`. `cargo xtask arch` keeps enforcing the
   direction.

## Consequences

- Adding an action is shipping a plugin crate (or extending one): it is
  registered by the aggregation crate and the router picks it up, subject to
  the A402 registry check.
- The node's crate shrinks to composition, transports, and node services; its
  `actions/` module keeps the router, dispatch, guard, and authorization.
- Behavior is unchanged: the same handlers, the same registry check, the same
  transaction per operation. `actions/tests.rs` and `cargo xtask fast` verify it.
- A plugin operation whose path is not a registered A402 surface fails the node
  startup with the same error the table used to.

## Rollback

Revert the change set. No persisted data changed shape; the only build change
is where the operation handlers live.