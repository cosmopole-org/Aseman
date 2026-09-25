# aseman-guest-sdk

Guest-side request construction for A405. The SDK accepts a workload credential and a
registered host operation, signs the exact body, and returns the fixed guest URL and
proof header. It intentionally has no creature, database, provider, namespace, or role
parameter: the server resolves those values from the authenticated workload binding.

Verify with `cargo test -p aseman-guest-sdk`.
