# Network, Federation, and Realtime

## Client gateway

HTTP is the default public protocol, implemented with a hardened asynchronous Rust HTTP stack and TLS. OpenAPI defines request/response compatibility. SSE carries one-way streams and WebSocket supports interactive terminal or bidirectional realtime sessions.

Cross-cutting middleware provides:

- Authentication and authorization.
- Request IDs, tracing, and structured errors.
- Body, rate, concurrency, and duration limits.
- Idempotency and replay protection.
- CORS/origin policy where applicable.
- Graceful connection draining.

Existing TCP and WebSocket protocols remain temporary compatibility adapters that translate into the same typed application commands. They contain no business logic.

## Federation identity and directory

Each node publishes a signed `NodeDescriptor` with:

- Stable node ID derived from or bound to its public key.
- Active keys, rotation epoch, and revocation metadata.
- Federation/client endpoints and supported transports.
- API/contract versions and capabilities.
- Available workload/runtime classes.
- Expiry and sequence number.

Every registered workload has a signed minimal descriptor containing workload identity, home-node ID, home-node address, public key, expiry, and revision. Any authenticated workload in the federation can resolve another workload's minimal descriptor by stable workload ID. This universal resolution does not reveal private creature/program metadata, capabilities, logs, presence, or data; bulk enumeration and extended metadata remain policy-controlled.

The home node remains authoritative. Caches use TTL, revision checks, and revocation updates.

Resolving identity never grants authority. Every lifecycle, terminal, signalling, data, network, or delegation action still requires an explicit capability and destination-node authorization.

## Cross-node operations

Federated envelopes contain request ID, source/destination node, authenticated subject, target, action, payload digest, timestamp, expiry, nonce, hop limit, protocol version, and signature.

Flow:

1. Resolve the target's home node.
2. Authorize the source request locally.
3. Send the signed idempotent envelope.
4. Authenticate the sending node.
5. Reauthorize subject/action/target at the home node.
6. Execute or enqueue a durable operation.
7. Return a signed response/operation ID.
8. Deduplicate retries and record an audit capsule.

All inbound requests, responses, updates, and directory records use the same verification rules. Retries use exponential backoff and circuit breakers. Hop limits prevent routing loops.

Nomad federation is not Aseman federation. Nomad connects infrastructure inside/between Nomad regions; Aseman federation connects independently administered Aseman node-clusters and authorizes application-level operations.

## Realtime

Narrow ports cover event publication, subscription, presence, delivery state, and replay. The production default is a durable event-bus provider fed through transactional capsule outbox records. An in-memory provider is limited to development/tests.

Events include stable ID, kind, producer, subject, creature scope, policy labels, sequence, timestamp, expiry, trace ID, schema version, and payload capsule/reference.

Federation relays only authorized events. Delivery is at-least-once with deduplication at consumers; sequence and checkpoint capsules support replay. Sensitive events are not broadcast merely because a workload is discoverable.
