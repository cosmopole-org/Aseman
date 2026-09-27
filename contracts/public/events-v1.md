---
status: ACCEPTED
owner: network/realtime
source_of_truth: this contract, A701, A707, ADR 0014
verification: cargo test -p aseman-public-http; cargo test -p aseman-node adapters::gateway_subs; cargo test -p aseman-node api::public_http
---

# Public event stream v1

## Endpoint

`GET /v1/events/{stream}?after={sequence}&maxEvents={limit}` serves A707 events as
Server-Sent Events. The bridge credential is the `Aseman-Bridge-Token` header; it is
never placed in a URL where access logs can retain it. `Last-Event-ID` is preferred
over `after` when both are present. It is the dense per-stream A707 sequence, not the
event UUID.

Admission requires exactly one `Aseman-Session` or `Aseman-Proof`. The edge invokes
the registered `topic.subscribe` action through the same A401/A402 application path as
A701, and the bridge token may only narrow the topics in its stored grant. The admitted
creature is retained by the connection; every event is checked against that scope before
delivery. Neither a query parameter nor event payload can select tenancy.

## Frames and replay

An event frame has:

- `id`: the decimal A707 stream sequence;
- `event`: the A707 `kind`;
- `data`: one JSON object containing `event`, the full A707 envelope, and
  `payloadBase64`, the bytes covered by its digest.

Clients reconnect with `Last-Event-ID`. When retention has removed required history,
the server emits one `resync` event with `oldestSequence` and `latestSequence`, then
closes. It never presents an incomplete replay as success. A read/provider failure emits
one structured `error` event and closes.

## Bounds and lifecycle

- `maxEvents` is 1 through 1,000 and defaults to 100.
- Storage reads are at most 100 events per batch.
- One concurrency permit is held for the connection lifetime.
- Idle streams close after 30 seconds; keepalives are emitted every 5 seconds.
- Stream names are 1 through 256 bytes and bridge-token headers 1 through 4,096 bytes.
- Rate, concurrency, origin, request-ID, TLS, and graceful-drain policy are shared with
  the A701 listener.

Bridge publications append the A707 event and PostgreSQL outbox row before the legacy
socket fan-out. The process-local subscribers remain a rollback compatibility effect;
they are not the authoritative production history.
