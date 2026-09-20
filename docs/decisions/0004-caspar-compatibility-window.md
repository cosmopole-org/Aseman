---
status: DECISION
owner: product/release
source_of_truth: this ADR
last_verified_commit: 800df24076c7
verification: compatibility manifest, warnings, telemetry, and removal ledger
---

# ADR 0004: Bounded Caspar compatibility window

## Status

Accepted 2026-09-19.

## Decision

Catalogued `caspar-node`, `casparctl`, `CASPAR_*`, package, route, and protocol aliases
remain for two consecutive Aseman minor releases and at least 180 days after the first
stable Aseman release containing their replacement, whichever is longer. Security
fixes may disable an unsafe alias earlier with a documented emergency migration.

Aliases live only at composition/transport/config edges, emit actionable warnings,
have telemetry that contains no secrets or tenant data, link to a removal-ledger row,
and translate immediately to canonical Aseman types. A new feature is never added only
to the legacy surface. Conflicting old/new configuration fails with a clear error.

Removal requires the replacement and deletion gates: parity fixtures, published
migration guidance, observed usage below the release threshold for one full minor
release, rollback evidence, and explicit release approval. The exact calendar date is
generated once the first stable Aseman release date exists.

## Rollback

Before expiry, restore the isolated alias adapter without restoring legacy business
logic. After an approved deletion release, rollback uses the prior signed release; it
does not reintroduce uncatalogued compatibility code.

Rejected: immediate removal and indefinite compatibility.
