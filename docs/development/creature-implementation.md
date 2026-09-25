---
status: CURRENT
owner: runtime/migration/P7-05
source_of_truth: modules/runtime/*, apps/aseman-node/src/drivers/vmm/host/vm_host_functions.rs, crates/aseman-contracts/src/guest_api.rs
verification: cargo xtask fast
---

# Implementing a creature for each runtime type

A **creature** is the ownership, authorization, and guest-data boundary for programs
and their workloads (see `docs/glossary.md`). A **program** is the deployable unit;
the node runs each program on one of the seven runtime types. This document is the
authoritative, current guide to implementing a creature for each runtime type so that
it communicates correctly with the host Aseman node through the **creature bridge**
host-function calls. It replaces the former executable sample tree under `examples/`.

Every runtime funnels creature-to-node communication through the same conceptual
bridge, so the mechanics are common and only the concrete ABI differs per runtime.
The authoritative machine-readable inventory is
[`docs/generated/current-routes.json`](../generated/current-routes.json)
(112 guest operations) and the runtime capability matrix is
[`docs/generated/current-runtime-matrix.md`](../generated/current-runtime-matrix.md).

## 1. The creature bridge model

- A workload never talks to storage, databases, providers, or other creatures
  directly. It issues a **host function call**; the node resolves the calling
  creature's identity from the VM runtime context (`resolve_host_hierarchy` in
  `apps/aseman-node/src/drivers/vmm/host/vm_host_functions.rs`), never from guest
  input.
- The generic call envelope is
  `{"op": "<operation>", "input": {…}}`. The node stamps the caller's
  `programId`/`machineId`/`creatureId`/`vmId` into the request; a guest that
  includes these fields cannot spoof them.
- Every call is **authorized** as its registered action (`unified-host-call {op}`)
  for the resolved creature (A402, `same_creature` gating). A guest-selected
  database, provider, role, or namespace is forbidden; tenancy is always resolved
  server-side from the authenticated workload-to-creature binding.
- Every guest write commits on its own (ADR 0021); `commitTrx` is a no-op success
  retained for SDK compatibility.
- Responses are JSON. Success is `{"ok": true, …}`; failure is
  `{"ok": false, "error": "<message>"}`. Unknown operations return
  `{"ok": false, "error": "unknown host call {op}"}`.

### 1.1 The host-call operation inventory

| Family | Operations |
|---|---|
| Output / logging | `output`, `consoleLog`, `vmLog` |
| Guest KV (per-creature guest database) | `dbOp` (`put`/`del`/`get`/`getByPrefix`), `stateOp`, `commitTrx` |
| Guest documents (per-creature JSON) | `putJson`, `getJson`, `getByPrefix`, `delKey`, `getLink`/`putLink`/`delKey` |
| Resource locks | `lockResource`, `unlockResource` |
| Outbound HTTP | `httpPost`, `httpRequest` |
| VM lifecycle (target in `targetVmId`) | `runVm`, `terminateVm`, `deleteVm`/`destroyVm`, `statusVm`, `vmEndpoints`, `execVm`/`execDocker`, `copyToVm`/`copyToDocker`, `copyFromVm`, `buildVmImage`/`buildDockerImage` |
| Program lifecycle (target in `targetProgramId`) | `deployEntity`/`deploy entity`, `updateProgram`, `deleteProgram`/`deleteOwnedProgram`, `getProgram` |
| Signaling / messaging | `signal`, `signalUser`, `signalGroup`, `joinGroup`, `plantTrigger` |
| Bridge / gateway subscription (ADR 0024) | `registerBridgeToken`, `revokeBridgeToken`, `publishUpdate` |
| Creature CRUD | `createCreature`/`createOwnedCreature`, `getCreature`, `listCreatures`, `updateCreature`, `deleteCreature`/`deleteOwnedCreature`/`removeCreature`/`removeOwnedCreature` |
| Program CRUD | `createProgram`, `listPrograms`, `listProgramMachines` |
| Store CRUD + access | `createStore`/`createOwnedStore`, `getStore`, `listStores`, `updateStore`, `deleteStore`/`removeStore`/`deleteOwnedStore`/`removeOwnedStore`, `createAccess`/`createOwnedAccess`, `deleteAccess`/`removeAccess`/`deleteOwnedAccess`/`removeOwnedAccess`, `listStoreAccess`, `listStoreMembers`/`readMembers`, `hasAccessToStore` |
| VM-scoped resource stores | `createResourceStore`/`createVmOwnedStore`, `updateResourceStore`/`updateVmOwnedStore`, `deleteResourceStore`/`deleteVmOwnedStore`, `getResourceStore`/`getVmOwnedStore`, `listResourceStores`/`listVmOwnedStores`, `createResourceEntity`, `deleteResourceEntity` |
| Finance / metering | `startHold`, `settleHold`, `releaseHold`, `reservePool`, `settlePool`, `releasePool`, `debitPool`, `transfer`, `consumeLock`, `lockToken`, `validateSign`, `publishFinanceCatalog`, `publishFinanceQuote`, `registerFinanceNode`, `retireFinanceNode`, `registerFinanceResource`, `reviewFinanceResource`, `retireFinanceResource` |
| Secrets (read-only for guests) | `secretGet`, `secretListGranted` |
| Micro / misc | `genId`, `execShellAction` (as the resolved creature), `nodeIdentity`, `grantLogin` (node-owner programs only), `verifyProgramExecution`/`elpifyProof` |
| Refused | `protocolApi`/`callProtocolApi` (`node.protocol.call` is `never`) |

The exact per-op request/response shapes live in
`apps/aseman-node/src/drivers/vmm/host/vm_host_functions.rs` (`host_fn_*`) and the
guest SDK crate `crates/aseman-guest-sdk`.

## 2. Runtime-by-runtime implementation guide

### 2.1 WebAssembly (`wasm`) — WasmEdge, in-process

- **Artifact:** `module.wasm`, compiled to the `wasm32-wasi` target. Default
  runtime; the only runtime enabled by default.
- **ABI:** a single guest import `env.hostCall(offset: u32, length: u32) -> i64`,
  where the arguments point at a UTF-8 JSON string in guest memory and the return
  value packs `(result_offset << 32) | result_length` in the same memory. The JSON
  envelope is the generic `{"op", "input"}` object. The guest must export `malloc`
  and a `run(arg: u64) -> i64` entrypoint (e.g. built with TinyGo).
- **Legacy single-purpose exports** (`output`, `console_log`, `plant_trigger`,
  `http_post`, `run_docker`, `exec_docker`, `copy_to_docker`, `signal_store`,
  `trx_put`, `trx_del`, `trx_get`, `trx_get_by_prefix`) remain for older creature
  SDK builds; new creatures should use `hostCall` exclusively.
- **How to write one:** implement a small SDK wrapper (see the historical WASM guest
  SDK pattern: a single `//go:wasmimport env hostCall` plus typed helpers for
  `dbOp`, `httpPost`, `lockResource`, `consoleLog`, `output`, …). Call
  `hostCall("output", …)` to set the execution result, or return it from `run`.
  See `modules/runtime/wasm/src/host_calls.rs` for the op table and
  `modules/runtime/wasm/src/runtime.rs` for the import registration.

### 2.2 JavaScript (`javascript`) — QuickJS, in-process

- **Artifact:** one self-contained `module.js` assigning
  `globalThis.update(inputJson)` (may be async). Bundle with esbuild `iife`; there
  is no loader or filesystem.
- **ABI:** the same op table as Wasm (the two must stay in step), transported as a
  string: `__caspar_hostCall(requestJson)` returns the response JSON string. The
  prelude provides `hostCall(op, input)` (accepts an object or JSON string and
  returns the parsed response) and `hostCallRaw`. Frozen globals expose identity:
  `caspar.machineId`, `caspar.programId`, `caspar.vmId`, `caspar.storeId`,
  `caspar.runtime`. `console.*` is routed to the VM log stream.
- **Limits:** `resources.ramMb` heap cap, 1 MiB stack cap,
  `maxExecTimeSeconds` interrupt handler, `terminateVm` interrupts, watchdog thread
  for host-call-blocked runs. Host calls are synchronous only.
- **How to write one:** `globalThis.update` reads/writes per-creature guest
  documents via `hostCall("getJson", {key, path})` / `hostCall("putJson", {…})` and
  uses `console.log`. Output is the return value, or the `output` op (explicit
  `output` wins). See `modules/runtime/javascript/README.md` and the runnable
  `modules/runtime/javascript/examples/counter.js`.

### 2.3 Docker (`docker`) — out-of-process container

- **Artifact:** a `Dockerfile` (the entity file) whose image runs your creature
  server. No in-process ABI.
- **ABI:** the container connects over TCP to the **docker-host bridge gateway**
  (default `CASPAR_GATEWAY_HOST`/`CASPAR_GATEWAY_PORT`; reachable from inside the
  container via `host.docker.internal:host-gateway` on the shared bridge network).
  Wire protocol: `[u32 BE frame length][u8 op][u64 message_id][u64 correlation_id]
  [u32 seq][u32 total][payload]`, ops `0x01 HELLO` / `0x02 WELCOME` /
  `0x10 REQUEST {op,input}` / `0x11 RESPONSE` / `0x20 SIGNAL` /
  `0x30`/`0x31 PING`/`PONG` / `0x40 ERROR`. Identity is derived from the
  docker-network source IP of the connection (spoof-resistant) and mapped via
  `register_vm_container`/`identify_instance_by_ip`.
- **How to write one:** on startup dial the gateway, send `HELLO`, then exchange
  framed `REQUEST`/`RESPONSE` messages whose payloads are the generic `{"op",
  "input"}` envelope. The node proxies inbound HTTP to the container's in-container
  server (`forward_http` override). Implementations may use the historical Go
  gateway-client pattern that frames `hostCall(key, input)` JSON and echoes
  `textMessage` signals. See `modules/runtime/docker/src/controller.rs`.

### 2.4 Elpian (`elpian`) — AST interpreter, in-process

- **Artifact:** `module.elpian.json`, an Elpian AST program.
- **ABI:** a `host_call` AST node
  (`{"type": "host_call", "data": {"name": "<op>", "args": […]}}`) compiles to
  `askHost(apiName, args)`; the executor emits an envelope
  `{machineId, apiName, payload}` and `modules/runtime/elpian/src/runtime.rs`
  forwards it through the **unified host-call dispatcher** with node-stamped
  identity.
- **Capability-gated host APIs** include `log`, `host.send`, `host.request`,
  `gpu.*`, `vm.import`, `net.*`, `fs.*`, `time.*`, `random.*`, `task.*`. The
  guest's op name is mapped through the same dispatcher, so `{name, args}` must
  name a registered op from §1.1.
- **How to write one:** emit an AST whose `main` returns the result value and
  issues any side effects through `host_call` nodes. See
  `modules/runtime/elpian/crates/elpian-vm/src/{api.rs,sdk/vm.rs,sdk/compiler.rs}`.

### 2.5 Elpify (`elpify`) — provable VM, in-process

- **Artifact:** `module.elpify.js` (Elpify source) which the runtime transpiles to
  MASM (`module.masm`), executed with STARK proof generation.
- **ABI:** **pure computation only.** A guest program makes no host calls; the
  runtime reaches the host only for logs and context lifecycle. Proofs are
  returned to the node through `verifyProgramExecution`/`elpifyProof`, which the
  node verifies.
- **How to write one:** express the computation as a pure function of its inputs;
  there is no I/O, no clock, and no bridge surface available to the program. See
  `modules/runtime/elpify/src/` and `modules/runtime/elpify/crates/elpify-lang/`.

### 2.6 Firecracker (`fire`) — microVM, supervised process

- **Artifact:** `module.wasm` boot image for the microVM. `restorable: yes` (the
  node supervises the microVM process).
- **ABI:** no guest bridge ABI. The workload reaches the node only via the host's
  `dispatch(...)` packet router with `vmLog`/`signal` packets (e.g. emitting
  `fire.vm.output` signals) and via `register_vm_context`/`unregister_vm_context`.
  Output lines are surfaced as `signal` + `vmLog` packets.
- **How to write one:** the guest workload is a standalone OS/runtime inside the
  microVM; any host communication is out-of-band via the packet router, not a
  guest-embedded host-call. See `modules/runtime/firecracker/src/`.

### 2.7 Modal (`modal`) — cloud sandbox, out-of-process

- **Artifact:** a `Modalfile`. `restorable: yes`. The guest runs in Modal's cloud.
- **ABI:** no in-process ABI. Reachable host services come through the
  `/gateway/*` bearer-token bridge channel (see ADR 0024): a creature first calls
  `registerBridgeToken` to mint a token with topics/ttl, then external programs
  holding the token push updates via `publishUpdate`. Identity is kept in node
  state links (`ModalSandbox::<vmId>`, `ModalVolume::<vmId>`, `ModalApp::…`,
  `ModalImage::…`) via `state_apply_ops`; `forward_http` proxies through the
  sandbox tunnel; `vm_endpoints` publishes reachable endpoints.
- **How to write one:** build the sandbox to serve the node's forwarded HTTP and
  to consume bridge-token updates; use `registerBridgeToken`/`publishUpdate` for
  push. See `modules/runtime/modal/README.md` and `modules/runtime/modal/src/`.

## 3. Rules that apply to every runtime

1. Never trust or send a caller-chosen `programId`, `creatureId`, `vmId`,
   database, role, or namespace selector; the node stamps and resolves them
   server-side.
2. Keep every call inside the §1.1 inventory; an unknown op is refused.
3. Guest database and document operations are confined to the calling creature's
   guest database (`GuestDoc::<creature>::` namespaces, ADRs 0021/0028).
4. `protocolApi`/`callProtocolApi` are refused for all guests.
5. `grantLogin` and finance `publish*`/`register*` calls are node-owner programs
   only.
6. Use `consoleLog`/`vmLog` for diagnostics and `output` for the execution result.
7. A creature must remain runnable against a disposable deployment and must never
   embed credentials; secrets are read through `secretGet`/`secretListGranted`.

## 4. Where to look

- Host dispatcher and per-op shapes: `apps/aseman-node/src/drivers/vmm/host/vm_host_functions.rs`.
- Guest contract and target rules: `crates/aseman-contracts/src/guest_api.rs`.
- Guest SDK: `crates/aseman-guest-sdk`.
- Per-runtime op tables: `modules/runtime/wasm/src/host_calls.rs`,
  `modules/runtime/javascript/src/host_calls.rs`.
- Runtime matrix and op inventory: `docs/generated/current-runtime-matrix.md`,
  `docs/generated/current-routes.json`.
- ADRs: `docs/decisions/0001-creature-isolated-guest-databases.md`,
  `0021-legacy-guest-kv.md`, `0024-legacy-bridge-grants.md`,
  `0028-confined-guest-documents.md`.