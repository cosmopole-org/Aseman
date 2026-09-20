---
status: DECISION
owner: architecture/release
source_of_truth: this ADR
last_verified_commit: 800df24076c7
verification: packaging manifest and license-policy checks
---

# ADR 0002: Nomad integration without redistribution by default

## Status

Accepted 2026-09-19 as the safe distribution policy. It is an engineering policy,
not legal advice. HashiCorp currently documents Nomad CE under the Business Source
License; a separate recorded legal approval is required to broaden distribution.

## Decision

Aseman implements and tests a Nomad VMM-backend provider but does not bundle, mirror,
redistribute, or automatically download a Nomad binary or image. Operators supply a
compatible Nomad installation and accept its terms independently. Shipped compact
artifacts use the native provider until an operator explicitly selects a discovered
Nomad endpoint. Documentation calls Nomad the intended production provider, not an
unconditionally open-source or included dependency.

Release metadata records the tested Nomad versions and the provider refuses unknown
major contract/API versions unless an override is explicitly acknowledged. A future
distribution change requires legal approval, SBOM/license updates, and an amendment to
this ADR; the provider boundary remains unchanged.

## Consequences, migration, rollback

The product can develop Nomad support without making a license claim or shipping a
third-party executable. Installation is less automatic. Bootstrap diagnoses the
missing external dependency and provides operator instructions. Rollback selects the
native provider after workload/volume compatibility checks; it never installs or
removes the operator's Nomad deployment.

Rejected: bundling Nomad CE before approval; abandoning the provider solely because
of its license; or hiding installation in a script.
