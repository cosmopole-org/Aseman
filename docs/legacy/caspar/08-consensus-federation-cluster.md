---
status: LEGACY
owner: compatibility/caspar
source_of_truth: docs/README.md
---

# 08 — Consensus, Federation & Cluster

This page covers the distributed-systems layers: the hashgraph consensus chain,
the STARK-attested validator election, federation between deployments, sharding,
and the geo-distributed instance mesh. Each is its own section.

---

## Hashgraph chain (Babble)

Consensus is an **embedded Babble (Rust) hashgraph** — leaderless, asynchronous,
Byzantine-fault-tolerant. It lives in `apps/aseman-node/src/drivers/network/chain` and runs
as a service on `BLOCKCHAIN_API_PORT`. The node registers a `commit_handler`
that receives ordered blocks of transactions and fans them out to application
state.

**Consensus routing** decides what goes through the chain, by the input's
`origin` field (not the route name):

- `origin == "global"` → consensus-bound; produces `Adding Transaction →
  Commit block=N`.
- `origin == ""` → local (reads, `creatures.signal`, dev `login`); no chain
  activity.

The chain HTTP surface exposes `/stats`, `/peers`, `/validators`, `/history`,
which the telemetry collector proxies into the snapshot's `chain`/`validators`/
`staking`/`election` fields (visible in `casparctl stats`).

Measured behaviour (reference run `reports/final`): median consensus round
≈95 ms (84 ms floor), peak sequential throughput ~10.5 ops/s for
`chain:submitBaseTrx`.

---

## Consensus quiet-state & which actions need it

A fresh node that sits **without consensus-bound traffic goes completely
silent** after the `BABBLING` banner — no `Self-Event`, no `Decided Round`, no
`Commit`. This is not a deadlock. `Node::babble()` waits on the heartbeat tick;
each tick calls `dispatch_gossip()`, which for a single validator picks no peer
(the selector excludes self) and falls through to `monologue()`. `monologue()`
only emits a self-event when `core.busy()` — i.e. when there are pending
transactions or pooled signatures. With nothing to order, every tick is a
silent no-op: the chain is alive, it just has nothing to do.

Push **one** consensus-bound write at it (any action whose input declares
`origin == "global"`) and the log lights up:
`Adding Transaction → Created Self-Event → Decided Round → Commit block=N`.

| action | needs consensus? | signal in node.log |
|--------|------------------|--------------------|
| `creatures.me` / `creatures.get` / `creatures.list` | no (read-only) | none |
| `creatures.signal` | no (async dispatch via the signaler) | none — wasm gas only |
| `creatures.login` (dev) | no (creates the account directly) | none |
| `creatures.createMachine` | yes (`origin == "global"`) | Adding Transaction + Commit |
| `creatures.create` | yes | Adding Transaction + Commit |
| `programs.create` | yes | Adding Transaction + Commit |
| `programs.deploy` | yes — and slow (decodes wasm bytes, writes to disk, builds the entity) | Adding Transaction + Commit + a large WasmEdge gas block |

**Single-validator caveat.** A solo node commits blocks locally but logs
`Block is not a suitable Anchor … trust_count=0` after every commit — blocks
never become anchor blocks because there is no second signer. This is fine for
development; real safety needs multiple validators (a shard) or federation.
Heavy transactions (notably `/programs/deploy`) run synchronously on the babble
proxy thread, so a bursty deploy workload serialises behind that pipeline.

---

## elpify-chain (STARK validator election)

The elpify-chain creature runs a five-phase **commit-reveal Proof-of-Stake**
election entirely inside WASM, attested by Miden **STARK** zero-knowledge
proofs:

1. **Stake** — record `(id, stake)`.
2. **Commit** — validators submit `h = H(s ‖ n)`.
3. **Reveal** — validators submit `(s, n)`; the creature checks the hash and
   accumulates VRF input.
4. **electionTick** — a MASM program runs in the elpify VM; the Miden prover
   (the `elpify-lang` crate) emits a STARK proof; winners are selected and the
   proof is broadcast via `signalGroup`.
5. **Validator consensus** — electors verify the succinct proof and finalise.

Proving is asymptotically `O(n log² n)` and verification `O(log² n)`, so adding
a validator is far cheaper than generating the proof. See the `elpify` runtime
in [VM Types](07-vm-types-and-implementation.md#elpify--elpify-provable-vm).

---

## Federation

Each node runs a federation transport on `FEDERATION_API_PORT` that validates
the **signature and origin** of inbound packets against a known-origins registry
before admitting them to the local action router. Outbound calls propagate
creature updates and chain events to registered peers.

This enables **cross-deployment creature composition** and **cross-shard event
delivery without replicating consensus history** — a node can compose with
creatures on an independent deployment by federating requests/updates rather
than sharing a chain.

---

## Sharding

A **shard** is a self-contained three-node Babble consensus group with no
cross-shard coordination, so aggregate throughput is additive:

```text
TPS_network = S × TPS_shard
```

The chain API exposes `chains/createShard` and `chains/registerNode`; each shard
runs its own Babble instance. A single shard is the three local nodes
(8074 / 8174 / 8274) sharing one Babble group.

---

## Geo-distributed cluster

The Caspar network is a **two-layer hierarchy**:

1. **Outer layer — the network of nodes.** Independent nodes (different
   origins/authorities) compose the decentralised network through the hashgraph
   BFT chain and the federation bridge.
2. **Inner layer — the instances of one node.** A single logical node can be
   made of multiple replicated **instances**, each on a separate server
   anywhere, replicated with **OpenRaft**. From the outer network's view the
   instances are still *one node* (chain and federation identity unchanged).

The inner layer is **edge-first**: the instance that receives a request executes
it immediately against its local RocksDB (reads never leave the instance), then
the committed write-set is proposed to the raft log and applied on every other
instance. Followers forward proposals to the leader, so requests can land on any
instance. The trade-off is last-writer-wins between instances mutating the same
key concurrently.

### What replicates

| Source | Replicated? | Mechanism |
|--------|-------------|-----------|
| Shell-API writes (users, machines, programs, stores, …) | always | write-set → `KvBatch` raft entry |
| Creature deployed with `distribution: "cluster"` | always | full artifact → `Deploy` raft entry |
| VM state of cluster-distributed creatures (storage ops, per-VM txns) | always | write-set → `KvBatch` raft entry |
| VM state of local-mode creatures (`distribution: "local"`, default) | never | commits on the receiving instance only |
| Cluster-wide operator knobs (`extra.*` config keys) | always | `ConfigPut` raft entry |

The hashgraph/babble chain, federation transport, telemetry, and QuestDB keep
their per-node behaviour; the raft mesh replicates the shell/VM key-value state.

### Enabling cluster mode

Off by default. Enable via `<STORAGE_ROOT_PATH>/cluster/cluster.json`
(or `CLUSTER_CONFIG_PATH`), or environment variables:

```bash
CLUSTER_ENABLED=true
CLUSTER_BOOTSTRAP=true            # ONLY on the first (seed) instance
CLUSTER_NODE_ID=1                 # unique u64 per instance
CLUSTER_NODE_NAME=caspar-eu
CLUSTER_REGION=eu-west
CLUSTER_LISTEN_ADDR=0.0.0.0:7440  # raft RPC + orchestration API
CLUSTER_ADVERTISE_ADDR=caspar-eu.example.com:7440
CLUSTER_AUTH_TOKEN=shared-secret  # optional, protects the listener
```

**Exactly one instance** of a brand-new cluster boots with
`CLUSTER_BOOTSTRAP=true` (it becomes the first voter/leader); every joining
instance stays pristine and is pulled in when the seed runs
`casparctl cluster add-peer` — a pristine instance must never self-initialise or
it forms a second, un-mergeable cluster. `casparctl cluster init` seeds manually.

### Distributed vs local deployment

`/programs/deploy` takes a distribution choice
(`"distribution": "cluster" | "local"`; default `local`):

- **`cluster`** — the program (artifact bytes included) is written to the raft
  log and materialised on **every instance** (files saved, records/links
  restored, VMM listener registered, and for build-on-deploy runtimes like
  docker the image built locally on each instance). Any instance can then serve
  the creature's VM requests (Cloudflare-Workers-style geo execution), and its
  key-value mutations propagate back through consensus.
- **`local`** — the creature exists only on the receiving instance; nothing
  about its VM execution enters consensus.

### Orchestration & design notes

Orchestrate with `casparctl cluster …`
(full reference in [Casparctl](04-casparctl.md#casparctl-cluster)):

```bash
casparctl cluster status                                 # leader / membership / RTT
casparctl cluster init --include-peers                   # bootstrap on this node
casparctl cluster add-peer --id 2 --addr eu.example.com:7440 --region eu-west
casparctl cluster add-peer --id 4 --addr edge.example.com:7440 --learner  # read replica
casparctl cluster nearest                                # peers by measured RTT
casparctl cluster promote --ids 1,2,3                    # set raft voters
casparctl cluster config set heartbeat_interval_ms 250   # one knob
casparctl cluster apply -f cluster.json                  # whole-cluster config
```

- **Raft storage** lives in a dedicated RocksDB at
  `<storage_root>/cluster/raft-db` (column families `meta`, `logs`, `sm`),
  separate from the node's main database.
- **Snapshots & joiners.** Join new instances before log compaction trims
  history (`snapshot_logs_since_last`, default 5000), or seed them from a
  storage backup of an existing instance first.
- **Security.** With `CLUSTER_AUTH_TOKEN` set, every raft / orchestration call
  must present the `x-caspar-cluster-token` header; run the listener over a
  private network / VPN or terminate TLS in front of it. Set
  `CASPAR_CLUSTER_TOKEN` (or `--token`) on the CLI to match, and
  `CASPARCTL_CLUSTER_ENDPOINT` (or `--endpoint`, default
  `http://127.0.0.1:7440`) to target a remote listener.
- **Geo routing.** Each instance probes peers every `rtt_probe_interval_secs`
  and serves a latency-sorted table at `/cluster/nearest`
  (`casparctl cluster nearest`) — feed it to your GeoDNS/anycast layer.

---

## How the layers relate

- **Chain (Babble)** orders consensus-bound writes within a shard.
- **Shards** scale throughput horizontally with no coordination between them.
- **elpify-chain** elects the validator set that secures the chain, proven by
  STARKs.
- **Federation** links independent deployments/shards for composition and event
  delivery without sharing consensus.
- **Cluster (OpenRaft)** replicates one origin's shell state and opt-in VM state
  across geo-distributed instances for edge availability.
