---
status: DECISION
owner: storage/migration
source_of_truth: this ADR
last_verified_commit: a3212a7
verification: blob-store conformance on the local provider; file-backed family conformance on both adapters
---

# ADR 0027: File bytes stay in the node storage root behind a `BlobStore` port

## Status

Accepted 2026-09-21 (approved by the project owner). It completes ADR 0026 for the
file-backed core families.

## Context

Four core families point at file bytes:
- `core.file`
- `core.entity_artifact`, the deployed entity files
- `core.vm_resource_entity`, the resource entity data
- the entity and proxy configuration written with them

The A308 export already records each file as copy evidence: a store key, a SHA-256
digest, a size, and a media type. The plan does not name where file bytes live once
PostgreSQL is authoritative for these records. Legacy writes them under the node's
storage root (`save_data_to_global_storage` and related calls). They are node-local, as
the rest of the legacy node is.

## Decision

1. **Port.** File bytes are reached only through a `BlobStore` port: put bytes under a
   key, read them, check that they exist, and delete them. A put returns the blob's
   evidence (key, digest, size, media type).
2. **Phase 3 provider.** The only provider is the node's storage root, exactly where
   legacy keeps the bytes today.
   - A store key is the path relative to the storage root, so migrated evidence and new
     writes name bytes the same way, and no bytes move at cutover.
   - The migration runner's copy evidence for existing files may name the files in
     place.
3. **Records hold evidence only.** Capsules of the file-backed families carry the store
   key, digest, size, and media type, never the bytes (ADR 0016). A record whose
   bytes are missing is written only as an attested absence (`artifact_present = false`),
   as the export already does.
4. **Routing.** The file-backed record families route like every other core family
   (ADR 0026). The blob provider is chosen separately and stays the storage root in
   Phase 3, whichever provider serves the records.
5. **Later providers.** A replicated or object-store provider is added with the phase
   that owns artifact distribution (P5 VMM artifacts, P6 worker topology) behind the
   same port. It needs no change to the records, because store keys stay provider
   neutral.

## Consequences

- File bytes remain node-local after cutover, with the same availability as today.
  Losing a node's disk loses its files, as it does now.
- Deploy and resource paths that write files must go through the port, so a record and
  its evidence are always written together.

## Rejected alternatives

- Storing bytes in PostgreSQL: it bloats the core database with build artifacts and
  VM data, and it would need a copy of every file at cutover.
- Deferring the file-backed families past Phase 3: that would leave core families on
  legacy and fail the "PostgreSQL is the default" gate clause.
- Introducing an object store now: it adds an operational dependency before the phase
  that owns artifact distribution defines its requirements.
