# Add a port and adapter

Define the smallest behavior in `aseman-ports` using domain values and structured
errors. Document consistency, idempotency, deadline/cancellation, ordering, resource
bounds, and capability semantics. Implement concrete I/O in an adapter/provider, add
contract tests shared by all implementations, and wire it only in a composition root.

Never expose a database connection, provider query, transport request, runtime handle,
or global singleton through a port.
