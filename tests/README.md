# Product verification

`tests/` verifies the Aseman product. A failure here means the implementation or a
product contract is wrong.

- `contracts/` — reusable conformance kits (`aseman-*-conformance`) that every
  provider-independent behavior must reproduce, plus golden finance fixtures.
- `characterization/` — frozen legacy-surface and boundary suites (artifact A008).
- `migration/` — the A309 live legacy-to-PostgreSQL migration end-to-end proof.
- `evals/agent/` — cold-start repository-comprehension evaluations: whether a
  contributor can locate the right owner, invariant, and verification command.
  These measure repository comprehensibility rather than runtime behavior, so they
  are gated separately by `scripts/check_agent_evals.py`, but they are still
  verification and therefore live under `tests/`.

The `tests/contracts/*` crates are workspace members and are consumed as
dev-dependencies by the concrete providers that must satisfy them.