---
status: DECISION
owner: module-platform/security
source_of_truth: this ADR and contracts/module
last_verified_commit: working-tree
verification: A204 schemas, aseman-module-runtime tests, sample-provider lifecycle test
---

# ADR 0015: Signed module artifacts and fail-closed execution permissions

## Status

Accepted 2026-09-20.

## Decision

An installable module is a digest-pinned OCI artifact or deterministic `.amod` bundle.
The bundle contains the exact manifest, executable, configuration schema, SBOM, and
license manifest. An enrolled Ed25519 publisher signs a domain-separated message that
binds both the complete artifact digest and the exact manifest digest. The supervisor
rejects unknown or revoked publishers, malformed paths, missing required files,
digest/signature disagreement, unsupported platforms, and cache collisions before any
module code runs. Trust enrollment is an explicit administrative operation and never
uses unattended trust on first use.

The signed manifest declares network egress/listeners, read-only and read-write mounts,
and opaque secret references. The supervisor passes references, never secret values, to
a trusted platform launcher. That launcher must either deny the request or construct an
operating-system sandbox and return an enforcement receipt. Staging fails closed unless
the receipt exactly matches the signed permission declaration and confirms resolution of
exactly the requested secret references. Launchers must not broaden permissions, mount
the artifact writable, place secret values in arguments or persistent snapshots, or
claim controls the platform did not apply.

The verified cache is content addressed by the digest of the complete envelope. Writes
use unique same-directory temporary files, durable file flushes, and atomic rename;
existing entries are accepted only after rehashing. Bootstrap snapshots contain public
trust roots and secret references only, are independently signed, expire, and are a
derived recovery cache rather than authority.

## Migration and rollback

Legacy in-process providers remain available behind their existing ports until an
equivalent signed provider passes conformance and its permission launcher is available.
Activation changes only a routing generation. Rollback restores the previous verified
artifact and routing generation; it never restores a revoked publisher or bypasses a
failed permission receipt. Cache and bootstrap formats remain versioned so the prior
supervisor can ignore rather than execute an unsupported artifact.

Rejected: unsigned local executables, manifest-only signatures, secret values in module
metadata, launcher best-effort permission handling, writable artifact execution, and
automatic publisher trust.
