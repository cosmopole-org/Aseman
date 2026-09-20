# Add or migrate a configuration key

1. Name the canonical `ASEMAN_*` key and typed field; do not read it in domain or
   application code.
2. Add schema constraints and a secret reference rather than a secret value.
3. If replacing a legacy key, add a single edge alias, fail on canonical/legacy
   conflict, emit a redaction-safe warning/counter, and link an ADR 0004 removal row.
4. Test absent, valid, invalid, conflict, and secret-redaction behavior.
5. Regenerate A003/A103 and run `cargo xtask fast`.
