---
status: DECISION
owner: security/application
source_of_truth: this ADR
last_verified_commit: 800df24076c7
verification: A402-A404 decision/property/conformance fixtures
---

# ADR 0008: Typed Rust capability evaluator as the initial policy engine

## Status

Accepted 2026-09-19.

## Decision

The initial provider is a deterministic Rust evaluator over the versioned Aseman
subject/action/resource/condition registry and durable capability grants. It is not an
embedded general-purpose scripting language. Every decision returns allow/deny,
matched grants, attenuation chain, policy/registry version, expiry/revocation facts,
and a stable reason code. Unknown actions, resources, conditions, versions, or provider
errors deny by default.

Delegation is intersection-only across requested rights, currently valid delegable
parent rights, administrator constraints, time, audience, and maximum depth. Evaluation
uses an explicit decision timestamp and bounded input; it performs no network or
storage I/O. Grant loading, identity verification, token issuance, and evaluation are
separate ports.

Alternative engines may be installed only behind the same policy provider contract and
must reproduce the normative fixtures, including negative and explanation cases. The
typed Rust provider remains the recovery fallback but cannot bypass provider epoch and
routing rules.

## Migration and rollback

Legacy boolean/access-list behavior is translated into explicit least-privilege grants;
ambiguous legacy values grant nothing. Shadow decisions compare old and new behavior
before enforcement. Rollback restores the preceding policy epoch and evaluator while
retaining audit and revocation state.

Rejected: hard-coded checks spread across handlers, fail-open external policy calls,
and adopting a DSL before the action/resource semantics are complete.
