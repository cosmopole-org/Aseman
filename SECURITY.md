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
- legacy compatibility remains isolated under ADR 0004 and the removal ledger.

See [`docs/architecture/threat-model.md`](docs/architecture/threat-model.md),
[`plan/migration/05-security-and-authority.md`](plan/migration/05-security-and-authority.md),
and the accepted decisions in [`docs/decisions/`](docs/decisions/). Security-boundary
changes require `cargo xtask full` and the relevant adversarial/conformance suites.
