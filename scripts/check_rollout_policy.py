#!/usr/bin/env python3
"""Validate A1003's machine-readable rollout and abort policy."""

import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
POLICY = ROOT / "contracts/deploy/rollout-policy.json"


def positive(mapping: dict, name: str) -> None:
    value = mapping.get(name)
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        raise ValueError(f"{name} must be a positive integer")


def zero(mapping: dict, name: str) -> None:
    if mapping.get(name) != 0:
        raise ValueError(f"{name} must be zero; correctness is not a budget")


def main() -> int:
    policy = json.loads(POLICY.read_text())
    if policy.get("schema_version") != 1:
        raise ValueError("unsupported rollout policy schema")
    shadow = policy["shadow"]
    positive(shadow, "minimum_duration_seconds")
    positive(shadow, "minimum_read_comparisons")
    positive(shadow, "minimum_mutation_comparisons")
    for name in (
        "semantic_mismatches",
        "unauthorized_acceptances",
        "duplicate_effects",
        "missing_audit_records",
    ):
        zero(shadow["abort"], name)
    positive(shadow["abort"], "maximum_p95_regression_percent")

    canary = policy["canary"]
    stages = canary.get("traffic_percent_stages")
    if stages != sorted(set(stages or [])) or not stages or stages[-1] != 100:
        raise ValueError("canary stages must be unique, increasing, and end at 100")
    positive(canary, "minimum_stage_duration_seconds")
    positive(canary, "minimum_requests_per_stage")
    for name in (
        "unauthorized_acceptances",
        "duplicate_effects",
        "storage_semantic_mismatches",
        "unbalanced_finance_records",
    ):
        zero(canary["abort"], name)
    for name in (
        "maximum_error_rate_increase_basis_points",
        "maximum_p95_regression_percent",
        "maximum_not_ready_percent",
    ):
        positive(canary["abort"], name)

    rollback = policy["rollback"]
    for name in (
        "retain_previous_signed_release",
        "retain_storage_rollback_generation",
        "require_audit_correlation",
        "require_operator_and_timestamp",
    ):
        if rollback.get(name) is not True:
            raise ValueError(f"rollback.{name} must be true")
    commands = rollback.get("verification_commands")
    if not isinstance(commands, list) or not commands or not all(isinstance(v, str) and v for v in commands):
        raise ValueError("rollback verification commands are required")
    print("rollout policy passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        print(f"rollout policy failed: {error}", file=sys.stderr)
        raise SystemExit(1)
