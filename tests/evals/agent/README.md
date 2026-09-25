# Agent comprehension evaluations

These evaluations test whether a cold-start contributor can identify the correct
owner, invariant, files, and verification command without relying on chat history.
Cases are versioned in `cases.json`; expected answers name authoritative repository
paths rather than prose copied into this directory.

Run `python3 scripts/check_agent_evals.py` or `cargo xtask fast`.
