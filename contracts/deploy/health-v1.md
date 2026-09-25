# A904 health, readiness, and dependency contract

All service probes return a structured `HealthReport` and reveal no secrets.

- `/health/live` answers only whether the process event loop can respond. It performs no
  network, database, scheduler, or provider call and is never a restart cascade.
- `/health/ready` reports `api_ready` and `singleton_eligible` separately. Load balancers
  use only `api_ready`; singleton schedulers also require `singleton_eligible` and must
  still acquire the fenced lease before work.
- Every dependency has a stable name, `api`, `singleton`, or `both` scope, required flag,
  health flag, and bounded public reason. A failed required check affects its scope. A
  failed optional check remains visible as degradation but does not remove the replica.
- A dependency timeout is a failed check. Probe evaluation itself is bounded and never
  waits indefinitely. Responses are `Cache-Control: no-store`.
- Readiness grants no authority and cannot substitute for authentication, authorization,
  a coordination lease, or a fencing token.

The executable state calculation and its split-brain cases live in
`aseman_observability::HealthReport`. VMM already exposes separate live/ready endpoints;
node composition must use the same response semantics when its canonical listener is
activated.
