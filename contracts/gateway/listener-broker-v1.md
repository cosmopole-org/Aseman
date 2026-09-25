---
status: CURRENT
owner: network/node-shell
source_of_truth: this contract and aseman-domain::listener
verification: cargo test -p aseman-domain listener
---

# A703 listener broker semantics (v1)

The listener broker owns endpoint binding generations; protocol adapters own only
framing. A candidate is staged and proven ready before an atomic activation. Activation
makes the candidate the sole receiver of new connections and moves the previous active
generation to `draining`. A draining generation serves existing connections until it
reports empty or its bounded deadline expires, then stops.

Generations are positive and strictly increasing. There is at most one active, staged,
or draining generation for a listener. An endpoint may not be empty. Staging or readiness
failure leaves the active generation untouched. Failure after activation invokes
rollback: the draining generation becomes active again and the failed generation is
retired. If no previous generation exists, failure leaves the listener unavailable
rather than silently binding a different endpoint.

Every transition is idempotent for the generation already in the requested state and
rejects a different or stale generation. Binding failure, readiness failure, drain
timeout, and rollback are observable outcomes. A process restart reconstructs the last
committed generation from typed configuration and durable routing state; it never infers
authority from a socket that happens to be open.

The executable state machine is `aseman_domain::listener::ListenerBroker`.
