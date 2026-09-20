---
status: DECISION
owner: module-platform
source_of_truth: this ADR
last_verified_commit: 800df24076c7
verification: A202/A203 schemas and module conformance suite
---

# ADR 0003: Protobuf module RPC with explicit compatibility negotiation

## Status

Accepted 2026-09-19.

## Decision

The supervisor control protocol and non-HTTP provider data protocols use versioned
protobuf/gRPC over a Unix socket on one host or mutually authenticated TLS across
hosts. The node-facing VMM API remains its separately specified HTTP/OpenAPI contract.
Network adapters translate public protocols to a canonical gateway gRPC contract.

Every connection first exchanges protocol major/minor, provider kind, implementation
version, capabilities, limits, schema digests, and instance identity. Major versions
are incompatible. Within a major version, senders may add fields and RPCs; receivers
must ignore unknown fields, preserve unknown enum behavior as `UNSUPPORTED`, and may
not change existing field numbers or meanings. Capability absence causes an explicit
unsupported result, never silent emulation with weaker guarantees.

Deadlines, cancellation, idempotency keys, request/trace identities, bounded messages,
backpressure, health, and structured errors are mandatory contract fields/semantics.
Generated clients/servers are the only module wire bindings used by application
adapters.

## Migration and rollback

Legacy in-process providers are wrapped behind ports, then moved behind the generated
contract. During one negotiated-major overlap, the supervisor may route to either
version through isolated adapters. Rollback restores the previous routing generation
and binary; stateful providers also follow their domain migration journal.

Rejected: Rust dynamic-library ABI, ad-hoc JSON RPC, and provider-private negotiation.
