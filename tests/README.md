# Product verification

`tests/` verifies the Aseman product: contracts, characterization, conformance,
migration, security boundaries, and cross-service behavior. A failure here means
the implementation or a product contract is wrong.

`evals/agent/` is intentionally separate. It measures whether a cold-start coding
agent can navigate the repository and identify the right owner, invariant, and
verification command. Those cases evaluate repository comprehensibility rather
than product runtime behavior, so merging them into `tests/` would blur ownership
and make product gates depend on an agent-evaluation dataset.
