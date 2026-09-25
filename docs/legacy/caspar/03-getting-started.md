---
status: LEGACY
owner: compatibility/caspar
source_of_truth: docs/README.md
---

# 03 — Getting Started

This page takes you from a fresh checkout to a running node, and points at the
lifecycle tooling an operator (or an AI agent) uses day to day. The node and
the operator CLI are pure **Rust** — no Go toolchain is required.

---

## Prerequisites

- **Rust** + Cargo (stable, edition 2021)
- A C toolchain (for RocksDB / WasmEdge native builds)
- **QuestDB** (telemetry / time-series over the PostgreSQL wire protocol) —
  `run-nodes.sh` can auto-download the QuestDB jar if Java is present
- **Docker** — only needed for the `docker` / `pc` / Firecracker runtimes and
  for the `casparctl`-managed container flow
- TLS certificate / key files for the client and federation transports

---

## Environment setup

```bash
cd node
cp sample.env .env
```

Key variables (see `node/sample.env` for the full list):

- **Identity:** `OWNER_ID`, `OWNER_PRIVATE_KEY`, `ORIGIN`
- **Client / peer ports:** `CLIENT_WS_API_PORT`, `CLIENT_TCP_API_PORT`,
  `FEDERATION_API_PORT`, `BLOCKCHAIN_API_PORT`, `ENTITY_API_PORT`, `VM_API_PORT`
- **Telemetry:** `TELEMETRY_API_PORT` (default `9099`), `TELEMETRY_DB_PATH`
- **Storage paths:** `STORAGE_ROOT_PATH`, `BASE_DB_PATH`, `APPLET_DB_PATH`,
  `SEARCH_INDEX_PATH`, `STORE_LOGS_DB`
- **Topology:** `IPADDR`, `ROOT_NODE`, `IS_HEAD`
- **VM cost knobs:** `VM_EXEC_COST_PER_SECOND`, `VM_RAM_COST_PER_MB_PER_MINUTE`,
  `VM_CPU_CORE_COST_PER_MINUTE`, `VM_DISK_COST_PER_GB_PER_MINUTE`

---

## Build

```bash
cd node
make build            # cargo build --release -> target/release/caspar-node
                      # also builds the caspar-keygen binary
```

Other targets: `make test`, `make lint` (clippy), `make doc`,
`make casparctl-install`.

To rebuild the node with a specific set of VM plugins compiled in, use
`build-dist.sh` (it runs `casparctl vms sync` first — see
[Casparctl](04-casparctl.md#casparctl-vms)):

```bash
./build-dist.sh                        # build with all enabled VM plugins
./build-dist.sh --disable-vm docker    # one-shot exclusion for this build
```

---

## Run options

### A) Local cluster (recommended) ✅

```bash
# from the repo root — starts a 3-node shard + QuestDB
./run-nodes.sh
...
./stop-nodes.sh
```

### B) `casparctl`-managed container flow

```bash
make -C node casparctl-install        # or: cargo install --path cmd/casparctl

casparctl install --name caspar-node  # full setup: docker/gvisor/certs/testnet
casparctl start
casparctl stats                       # live telemetry TUI
casparctl pause | resume | stop | uninstall | purge
```

Full command reference: [Casparctl](04-casparctl.md).

### C) Direct binary

```bash
cd node
./target/release/caspar-node
```

### D) `casparctl install --local` + `run` — local single node, no Docker ✅

The lightest path (works in a plain sandbox): runs the pre-built `dist/` node
with no Docker/gVisor/proxy. Split into a one-time install phase and a
repeatable run phase.

```bash
casparctl install --local    # once: check requirements, generate config
casparctl run --detach        # start QuestDB + node
casparctl status              # process/port/telemetry status
casparctl stop                # stop node + QuestDB
```

This node serves **plaintext** client transports, so connect the client CLI
with `CASPAR_TLS=0` (see [Client CLI](09-client-cli.md)). See
[Casparctl → local flow](04-casparctl.md#casparctl-install---local--run--status--stop-local-no-docker).

---

## Node lifecycle at a glance (for operators / agents)

| Goal | Command |
|------|---------|
| Install a node as a container | `casparctl install --name <n>` |
| Start / pause / resume / stop | `casparctl start` / `pause` / `resume` / `stop` |
| Live dashboard | `casparctl stats` |
| Profile the runtime | `casparctl pprof runtime\|heap\|flamegraph` |
| Choose VM runtimes | `casparctl vms list\|enable\|disable\|sync\|new` |
| Orchestrate the mesh | `casparctl cluster status\|add-peer\|apply\|config` |
| Remove / purge | `casparctl uninstall` / `casparctl purge` |

---

## Smoke checks 🧪

- Node starts without panic.
- Telemetry endpoint returns a snapshot: `GET /telemetry/snapshot` (`:9099`).
- Auth key route answers: `/auths/getServerPublicKey`.
- One consensus-bound write (e.g. `chains/submitBaseTrx`) produces a
  `Commit block=N` in the node log.

---

## Talk to the node

Use the **client CLI** to authenticate, manage creatures/programs, and deploy
VMs — see [Client CLI](09-client-cli.md):

```bash
cd client-cli && npm install && npm run build && npm install -g .
CASPAR_HOST=127.0.0.1 caspar-client login alice alice@example.com
caspar-client creatures.me
```

---

## Benchmarks

```bash
./bench-all.sh           # drives all 3 nodes; writes workflow_report.md + JSON
```

Curated runs are archived under `reports/`; the authoritative reference run is
`reports/final/`.
