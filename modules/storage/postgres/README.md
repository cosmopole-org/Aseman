# PostgreSQL storage provider

This module maps each accepted core capsule kind to its own native table in the
`aseman_core` schema. Typed columns, checks, foreign keys, and partial unique indexes
are generated from the capsule registry. `capsule_cbor` preserves the exact canonical
envelope; it is not a universal entity table and is checked against the typed columns
on every provider write.

The guest catalog stores bindings and schema definitions only. Guest payloads belong
in separate creature databases/namespaces and are implemented by P3-03.

Run static checks with `cargo test -p aseman-storage-postgres`. Live integration tests
use `ASEMAN_TEST_POSTGRES_URL` and an isolated disposable database.
