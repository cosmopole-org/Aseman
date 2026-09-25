# Agent comprehension evaluations

These evaluations test whether a cold-start contributor can identify the correct
owner, invariant, files, and verification command without relying on chat history.
Cases are versioned in `cases.json`; expected answers name authoritative repository
paths rather than prose copied into this directory.

They live under `tests/` because they are verification: a case fails when a named
authority path moves or a verification command stops working. The `check_agent_evals.py`
script validates the catalog mechanically, and the catalog answers are checked by
`cargo xtask fast` like every other test suite. Unlike the product suites in
`tests/characterization` and `tests/contracts`, these evaluate repository
comprehensibility rather than runtime behavior.