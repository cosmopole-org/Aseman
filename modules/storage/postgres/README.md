# PostgreSQL storage provider

This module maps each accepted core capsule kind to its own native table in the
`aseman_core` schema. Typed columns, checks, foreign keys, and partial unique indexes
are generated from the capsule registry. By default (ADR 0034) a row is flattened:
every field, including a document field (JSONB), is its own column and the envelope is
rebuilt and verified on read. With `ASEMAN_STORAGE_CAPSULE_MODE=on`, `capsule_cbor`
also packs the exact canonical envelope. Neither layout is a universal entity table.

The guest catalog stores bindings and schema definitions only. Guest payloads belong
in separate creature databases/namespaces (ADR 0001).

Run static checks with `cargo test -p aseman-storage-postgres`. Live integration tests
use `ASEMAN_TEST_POSTGRES_URL` and an isolated disposable database.
