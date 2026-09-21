# Capsule query contract (A302)

`query.schema.json` defines closed, bounded, provider-neutral queries. Raw provider
query text is always rejected.

## Relationship equality predicates (P3-06 extension)

A `compare` predicate may name a declared **relationship** instead of a field, for
example `{"op":"compare","field":"store","operator":"equal","value":{"type":"bytes",...}}`.
It is restricted to:

- the `equal` operator,
- a 16-byte capsule ID value (the related capsule's ID).

Any other operator or value fails with `invalid_query`. The extension lets typed
repositories list, for example, the memberships of one store through an indexed
foreign-key column, without traversal or a denormalized copy of the relationship.
