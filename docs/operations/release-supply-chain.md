---
status: PARTIAL
owner: release/operations
source_of_truth: contracts/release/policy-v1.json
verification: python3 scripts/check_release_policy.py --check
---

# Release supply-chain procedure

`contracts/release/policy-v1.json` is the A906 authority. It fixes the canonical
binary set, architectures, SPDX format, license boundary, vulnerability decision,
SLSA predicate, digest algorithm, signing identity, and CI permissions. The fast gate
rejects drift between that contract, Cargo metadata, the release workflow, and the
out-of-tree `build-dist.sh` destination.

## Build and publish

The tagged or manually dispatched `build-node.yml` workflow builds amd64 and arm64
artifacts under the runner's temporary directory. It must never commit generated
binaries back to `dist/`. Each architecture produces:

- `aseman-dist-{arch}.tgz`, created with sorted names, normalized ownership, the source
  commit timestamp, and timestamp-free gzip output;
- `aseman-dist-{arch}.spdx.json`, an SPDX 2.3 inventory of Cargo dependencies and every
  packaged file with SHA-256;
- `aseman-dist-{arch}.SHA256SUMS` for the archive, SBOM, and scan report;
- a Grype artifact-scan report, with high and critical findings blocking publication;
- a build-provenance attestation and an SBOM attestation bound to the archive digest.

Every reusable action is pinned to a full commit SHA. Checkout credentials are not
persisted and the workflow has read-only repository contents permission. OIDC and
attestation write permissions exist only to create signed artifact attestations.

## Verify before promotion

Download the workflow artifact without renaming its files, then verify checksums and
the repository identity:

```sh
sha256sum --check aseman-dist-amd64.SHA256SUMS
gh attestation verify aseman-dist-amd64.tgz --repo OWNER/REPOSITORY
python3 scripts/check_release_policy.py --bundle PATH/CONTAINING/BOTH_ARCHITECTURES
```

Promotion records the workflow run, source commit, artifact digests, attestation URLs,
scanner result, verifier identity, and timestamp. A digest-pinned deployment consumes
the promoted artifact; a tag alone is not an artifact identity.

## Migration and rollback

This is the replacement side of RL-018. The tracked compatibility `dist/` remains until
one tagged workflow run for both architectures is retained and independently verified,
install/image consumers use the promoted artifacts, and rollback to the previous signed
release is rehearsed. Deletion before those observations would violate ADR 0004.

The license check currently carries one explicit RL-014 exception for legacy runtime
packages that lack SPDX metadata. It expires at the Phase 10 deletion gate. The workflow
runs both a RustSec Cargo.lock audit and a high/critical packaged-artifact scan. A906
remains PARTIAL until a tagged run and its promotion/verification evidence are retained.
