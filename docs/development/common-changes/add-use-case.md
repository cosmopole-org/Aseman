# Add an application use case

1. Add or reuse domain values/state transitions in `aseman-domain`.
2. Define only the behavioral dependencies the use case needs in `aseman-ports`.
3. Implement orchestration and authorization in `aseman-application`.
4. Test allow, deny, conflict, retry/idempotency, and no-effect-on-failure behavior with
   in-memory port doubles.
5. Add transport DTO translation outside the application crate.
6. Run `cargo xtask fast`; update the owning current-call-path and removal rows.

Do not pass JSON values, concrete clients, environment values, or provider identifiers
through the use-case boundary.
