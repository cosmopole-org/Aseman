---
status: ACCEPTED
owner: realtime
source_of_truth: this contract, aseman-domain::realtime, ADR 0014
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain realtime; live_realtime on PostgreSQL 16
---

# A707: the realtime event envelope (v1)

## Two promises, stated plainly

**Delivery is at least once.** Not because it would be hard to do better, but because
nothing can do better across a crash between "the consumer processed it" and "the
consumer recorded that it did". Consumers checkpoint after processing and deduplicate on
the event ID.

**Ordering is per stream, not global.** A consumer of one subject needs its own events
in order; nobody needs a total order across every creature on the node, and promising
one would mean a single writer for the whole system.

## The envelope

| Field | What it is |
|---|---|
| `id` | The event's identity, and what consumers deduplicate on |
| `stream` | What it is ordered within |
| `creature_id` | The scope authorization is decided against — never anything in the payload |
| `kind`, `producer` | What it is and who produced it |
| `sequence` | Monotonic and **dense** within the stream, from 1 |
| `at_millis` | When it happened |
| `payload_digest` | `sha256:{hex}` |
| `retention` | `transient` (1 hour), `standard` (7 days), or `durable` (kept until a retention decision) |
| `version` | `1`. An unknown version is refused, not guessed |
| `idempotency_key` | Optional; lets a repeated publication collapse |

## Dense sequences

A gap would make a consumer wait forever for an event that is never coming. A repeat
would make replay ambiguous. So an append whose sequence is not the stream's next is
refused, and the producer learns it raced rather than leaving a hole.

## Checkpoints never move backwards

Moving one back would replay events the consumer has already acted on, turning
at-least-once into twice-on-purpose. A backwards checkpoint is refused and the stored
one does not move.

## Replay, and honest loss

A consumer asks whether it may still replay from where it stopped. If the events after
that point have been purged, it is told to resync rather than served a silently
incomplete stream.

## The outbox

An event is inserted in the same transaction as the application change that caused it,
together with its outbox row. An event that exists is therefore always one that will be
published — "it happened" and "it was announced" cannot disagree.

Publication is singleton work and takes a fenced lease (ADR 0013). Workers claim bounded
batches with `SKIP LOCKED` and a deadline: a worker that dies releases its batch by
expiry rather than by cleanup, and only the worker that claimed a batch may complete it.
An event that has failed too many times becomes a dead letter for an operator to look
at, rather than being retried forever.

## Discoverability is not delivery

A workload being resolvable in the federation says nothing about which events it may
see. Scope is the creature's. Sensitive events are not broadcast merely because a
producer is discoverable.
