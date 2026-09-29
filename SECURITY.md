# Security policy

Please report suspected vulnerabilities privately to the repository maintainers. Do
not open a public issue containing credentials, exploit details, private keys, tenant
identifiers, or production data.

Security-sensitive changes must preserve the accepted trust boundaries:

- every sensitive action passes the single policy decision path;
- proof freshness, audience, key epoch, revocation, and replay checks fail closed;
- workloads never choose their creature, provider, database, namespace, or role;
- secrets and bearer credentials are never logged or included in `Debug` output;
- federation destinations authenticate and authorize independently;
- every surface reaches an operation through the node's one router and its guard
  (ADR 0039).

See [`docs/architecture/threat-model.md`](docs/architecture/threat-model.md),
[`contracts/security/`](contracts/security/), and the accepted decisions in [`docs/decisions/`](docs/decisions/). Security-boundary
changes require `cargo xtask full` and the relevant adversarial/conformance suites.
