# Caspar Client CLI (`caspar-client`)

A thin TypeScript/Node.js client for a **Caspar node's signed binary action
protocol** (the "Caspar shell API"). Every command maps directly to a Caspar
shell action route (`/creatures/*`, `/programs/*`) — there is no dependency on
any hosted backend, billing service, or miniapp layer.

The directory also owns `aseman_client.py`, the Python compatibility client for the
same framed API. It was consolidated here from the former top-level SDK; reusable
guest-side contracts belong in `crates/aseman-guest-sdk`, and the per-runtime
creature-implementation guide lives in
[`docs/development/creature-implementation.md`](../../docs/development/creature-implementation.md).

With it you can:

- authenticate against a node (`login` / `logout`),
- manage **creatures** (identities/accounts) and send signals,
- create, deploy, run, and manage **programs** (the deployable VM units), and
- scaffold ready-to-deploy **VM project templates** for all seven Caspar
  runtimes (`vm.init` / `vm.types`): `wasm`, `javascript`, `docker`, `fire`,
  `elpian`, `elpify`, `modal`.

> Full command reference and tutorials:
> [`../../docs/legacy/caspar/09-client-cli.md`](../../docs/legacy/caspar/09-client-cli.md).

## Install

```bash
npm install
npm run build
npm install -g .      # exposes the `caspar-client` binary
```

Verify:

```bash
caspar-client help
```

## Connect to a node

The CLI talks to a Caspar node over TLS (WebSocket by default). Point it at
your node with environment variables:

| Variable            | Meaning                                   | Default     |
|---------------------|-------------------------------------------|-------------|
| `CASPAR_HOST`       | node host                                 | `127.0.0.1` |
| `CASPAR_PROTO`      | `ws` or `tcp`                             | `ws`        |
| `CASPAR_PORT`       | action port (ws: 8076, tcp: 8077)         | proto default |
| `CASPAR_TLS`        | `0` = plaintext `ws://`/TCP (direct-to-node, no proxy) | `1` (TLS) |
| `CASPAR_INSECURE`   | `1` to skip TLS verification (dev only)   | unset       |
| `CASPAR_SIGNAL_TIMEOUT_MS` | signal round-trip timeout          | `30000`     |

> A node serves plaintext `ws`/`tcp` (TLS is normally terminated by an nginx
> proxy). Connecting straight to a node — e.g. one started by `casparctl run` —
> requires `CASPAR_TLS=0`.

## Run modes

```bash
caspar-client                              # interactive shell
caspar-client creatures.me                 # single command
caspar-client --batch "creatures.me; programs.list 0 10"
caspar-client --batch-file ./commands.txt
```

## Quick start: deploy a VM to Caspar

```bash
caspar-client login alice alice@example.com          # authenticate
caspar-client vm.init wasm ./my-vm main              # scaffold a project
caspar-client creatures.createMachine 1 my-app "My app" "demo"   # -> creatureId
caspar-client programs.create ep <creatureId> /api/main wasm "entry"  # -> programId
caspar-client programs.deploy <programId> ./my-vm wasm '{}'      # build + deploy
caspar-client programs.run <programId>               # run it
```

See [`../../docs/legacy/caspar/09-client-cli.md`](../../docs/legacy/caspar/09-client-cli.md)
for the compatibility command reference, and
[`../../docs/legacy/caspar/07-vm-types-and-implementation.md`](../../docs/legacy/caspar/07-vm-types-and-implementation.md)
for how each of the six VM runtimes works.
