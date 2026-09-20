---
status: CURRENT
owner: module-platform
source_of_truth: crates/aseman-module-runtime and contracts/module/lifecycle.schema.json
verification: cargo test -p aseman-module-runtime
---

# Module lifecycle and routing generations

Allowed forward transitions are `installed → validated → staged → ready → active →
draining → stopped`. A failed signature, digest, manifest, conformance, start, or health
check cannot become routable. Start/readiness failures attempt stop and enter `failed`.
A stopped, already-conformant version may be staged again.

Activation atomically advances a monotonic routing generation and directs new work to
the ready candidate. The previous process enters `draining` but remains alive during
the rollback window. Rollback first requires that process to acknowledge `restore`,
then advances the generation again, restores that process,
and drains the failed candidate. A drained process may be stopped only after its
rollback window and capability-specific state migration permit it.

Stateful providers add their own migration journal and cutover gates. The generic
supervisor never treats observed process state as desired module state.
