# Aseman clients

The canonical public HTTP v1 clients are generated from
[`contracts/public/openapi.json`](../../contracts/public/openapi.json):

- [`generated/public-v1.ts`](generated/public-v1.ts) — dependency-free TypeScript
  using the platform `fetch` API.
- [`generated/public_v1.py`](generated/public_v1.py) — dependency-free Python using
  `urllib`.

Both expose all 76 published operations plus the bounded `events` SSE iterator,
enforce exactly one session/proof credential, and require an idempotency key before
sending a mutation. The iterator carries `Last-Event-ID`, parses event/resync/error
frames, and supports cancellation/timeout through each language's normal HTTP controls.
Regenerate them with
`python3 scripts/generate_public_clients.py`; freshness is part of `cargo xtask fast`.
The compatibility rules are versioned in
[`contracts/public/client-policy.json`](../../contracts/public/client-policy.json).

## The command-line client (`aseman-client`)

A thin TypeScript/Node.js client for an **Aseman node's signed-packet protocol**.
Every command maps directly to an operation route (`/creatures/*`, `/programs/*`) —
there is no dependency on any hosted backend, billing service, or miniapp layer.

The directory also owns `aseman_client.py`, a Python client for the same framed
protocol. Reusable guest-side contracts belong in `crates/aseman-guest-sdk`, and the
per-runtime creature-implementation guide lives in
[`docs/development/creature-implementation.md`](../../docs/development/creature-implementation.md).

With it you can:

- authenticate against a node (`login` / `logout`),
- manage **creatures** (identities/accounts) and send signals,
- create, deploy, run, and manage **programs** (the deployable VM units), and
- scaffold ready-to-deploy **VM project templates** for all seven Aseman
  runtimes (`vm.init` / `vm.types`): `wasm`, `javascript`, `docker`, `fire`,
  `elpian`, `elpify`, `modal`.

> The full command reference is `aseman-client help`.

## Install

```bash
npm install
npm run build
npm install -g .      # exposes the `aseman-client` binary
```

Verify:

```bash
aseman-client help
```

## Connect to a node

The CLI talks to an Aseman node over TLS (WebSocket by default). Point it at
your node with environment variables:

| Variable            | Meaning                                   | Default     |
|---------------------|-------------------------------------------|-------------|
| `ASEMAN_HOST`       | node host                                 | `127.0.0.1` |
| `ASEMAN_PROTO`      | `ws` or `tcp`                             | `ws`        |
| `ASEMAN_PORT`       | action port (ws: 8076, tcp: 8077)         | proto default |
| `ASEMAN_TLS`        | `0` = plaintext `ws://`/TCP (direct-to-node, no proxy) | `1` (TLS) |
| `ASEMAN_INSECURE`   | `1` to skip TLS verification (dev only)   | unset       |
| `ASEMAN_SIGNAL_TIMEOUT_MS` | signal round-trip timeout          | `30000`     |

> A node serves plaintext `ws`/`tcp` (TLS is normally terminated by an nginx
> proxy). Connecting straight to a node — e.g. one started by `asemanctl start` —
> requires `ASEMAN_TLS=0`.

## Run modes

```bash
aseman-client                              # interactive shell
aseman-client creatures.me                 # single command
aseman-client --batch "creatures.me; programs.list 0 10"
aseman-client --batch-file ./commands.txt
```

## Quick start: deploy a VM to Aseman

```bash
aseman-client login alice alice@example.com          # authenticate
aseman-client vm.init wasm ./my-vm main              # scaffold a project
aseman-client creatures.createMachine 1 my-app "My app" "demo"   # -> creatureId
aseman-client programs.create ep <creatureId> /api/main wasm "entry"  # -> programId
aseman-client programs.deploy <programId> ./my-vm wasm '{}'      # build + deploy
aseman-client programs.run <programId>               # run it
```

Run `aseman-client help` for the command reference, and see
[`docs/development/creature-implementation.md`](../../docs/development/creature-implementation.md)
for how each VM runtime works.
