---
status: ACCEPTED
owner: finance/enforcement
source_of_truth: this contract, aseman-domain::finance
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain finance
---

# A805: insufficient funds (v1)

## The steps

```text
balance >= 0                          -> none
below zero, within grace              -> notify
grace elapsed, within pause window    -> pause
grace and pause window both elapsed   -> stop
```

Each step is ordered, so a caller cannot skip one by comparison. Each is reversible by
paying.

## Why it escalates

**Nothing is immediate.** A balance that dips below zero between a charge and a top-up
is normal; stopping someone's workloads for it would be worse than carrying the debt for
an hour.

**Pause comes before stop.** Pausing preserves memory, so paying up resumes the running
process rather than restarting it. The pause is the runtime's own pause and may never
degrade to a stop and a start (A604) — a degraded pause that reported success would tell
the creature their process survived when it did not.

**Stop is not destroy.** The processes end; state on disk survives.

## How it acts

Through ordinary authorized VMM operations — the same A501 calls any other caller
makes, subject to the same policy, producing the same audit capsules. Enforcement has no
private path to the runtime, so a creature can see exactly what was done to their
workloads and by what authority.

## A zero balance is paid up

`none`, however long it has been zero. Nobody is enforced against for owing nothing.
