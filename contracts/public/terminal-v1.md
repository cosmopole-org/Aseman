---
status: CURRENT
owner: network/node-shell
source_of_truth: modules/network/http and contracts/gateway/v1/gateway.proto
verification: aseman-public-http and aseman-gateway-rpc tests
---

# Public terminal stream v1

`GET /v1/terminals/{workloadId}` upgrades to WebSocket subprotocol
`aseman.terminal.v1`. It requires exactly one `Aseman-Session` or `Aseman-Proof`, a
shaped `Idempotency-Key`, and the `creatureId` and `vmId` query members. Admission is
performed through the registered workload log action and the resolved typed workload
ID must equal the path ID before output is delivered.

Client text frames are JSON:

- `{"type":"stdin","data":"<base64>"}`
- `{"type":"resize","columns":120,"rows":40}`
- `{"type":"close"}`

Server text frames are JSON:

- `{"type":"output","sequence":n,"channel":"stdout|stderr|system|build","data":"<base64>"}`
- `{"type":"error","status":n,"reason":"...","detail":"..."}`
- `{"type":"exit","code":n}`

Connections count against the listener concurrency limit and poll in bounded batches.
The current native runtimes expose the ADR-0029 legacy terminal as an authorized log
stream, so output and resumable sequence delivery work while stdin and resize return
`unsupported_operation`. A future runtime may advertise an interactive PTY only after
its A501/A504 provider implements those frames; no capability is silently emulated.

Network modules use the identical terminal semantics through A702's bidirectional
`Gateway.Terminal` RPC.
