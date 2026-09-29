#!/usr/bin/env python3
"""Check A708 realtime capacity and A905 observability artifacts as one contract."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
POLICY = ROOT / "contracts/observability/policy-v1.json"
CAPACITY = ROOT / "contracts/realtime/capacity-v1.json"
DASHBOARD = ROOT / "deploy/observability/grafana/aseman-overview.json"
ALERTS = ROOT / "deploy/observability/prometheus/aseman-alerts.yml"
RUNBOOK = ROOT / "docs/operations/observability.md"
REALTIME = ROOT / "crates/aseman-capsule/src/realtime.rs"
MODELS = ROOT / "contracts/capsule/kinds/core-logical-schemas.json"

METRIC_RE = re.compile(r"\baseman_[a-z0-9_]+\b")
ALERT_RE = re.compile(r"^\s*- alert: ([A-Za-z][A-Za-z0-9]+)\s*$", re.MULTILINE)
RUNBOOK_RE = re.compile(r"runbook_url:\s*docs/operations/observability\.md#([a-z0-9-]+)")


def fail(message: str) -> None:
    raise SystemExit(f"observability policy: {message}")


def load(path: Path) -> dict:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot load {path.relative_to(ROOT)}: {error}")
    if not isinstance(value, dict):
        fail(f"{path.relative_to(ROOT)} must contain a JSON object")
    return value


def metric_base(name: str, known: set[str]) -> str | None:
    if name in known:
        return name
    for suffix in ("_bucket", "_count", "_sum"):
        if name.endswith(suffix) and name[: -len(suffix)] in known:
            return name[: -len(suffix)]
    return None


def check_metric_references(text: str, known: set[str], source: str) -> None:
    unknown = sorted({name for name in METRIC_RE.findall(text) if metric_base(name, known) is None})
    if unknown:
        fail(f"{source} references unknown metrics: {', '.join(unknown)}")


def main() -> None:
    policy = load(POLICY)
    capacity = load(CAPACITY)
    dashboard = load(DASHBOARD)
    alerts_text = ALERTS.read_text()
    runbook_text = RUNBOOK.read_text()

    if policy.get("artifact") != "A905" or capacity.get("artifact") != "A708":
        fail("artifact IDs must be A905 and A708")
    metrics = policy.get("metrics")
    if not isinstance(metrics, list) or not metrics:
        fail("A905 must define a non-empty metric catalogue")
    names = [metric.get("name") for metric in metrics]
    if len(names) != len(set(names)) or any(not isinstance(name, str) for name in names):
        fail("metric names must be unique strings")
    known = set(names)
    allowed = set(policy["labels"]["allowed_bounded"])
    forbidden = set(policy["labels"]["forbidden_high_cardinality"])
    for metric in metrics:
        name = metric["name"]
        if not name.startswith("aseman_"):
            fail(f"metric {name} is outside the aseman_ namespace")
        labels = set(metric.get("labels", []))
        if labels - allowed:
            fail(f"metric {name} has undeclared labels: {sorted(labels - allowed)}")
        if labels & forbidden:
            fail(f"metric {name} carries high-cardinality labels: {sorted(labels & forbidden)}")
        if metric.get("type") not in {"counter", "gauge", "histogram"}:
            fail(f"metric {name} has an unsupported type")

    slos = policy.get("slos", [])
    if len(slos) < 4 or len({slo.get("id") for slo in slos}) != len(slos):
        fail("A905 must define at least four uniquely named SLOs")
    for slo in slos:
        objective = slo.get("objective")
        if not isinstance(objective, (int, float)) or not 0 < objective < 1:
            fail(f"SLO {slo.get('id')} has an invalid objective")
        for key in ("good_query", "total_query"):
            query = slo.get(key)
            if not isinstance(query, str) or not query:
                fail(f"SLO {slo.get('id')} lacks {key}")
            check_metric_references(query, known, f"SLO {slo.get('id')}")

    if dashboard.get("uid") not in policy.get("dashboards", []):
        fail("the checked dashboard UID is absent from the A905 policy")
    panels = dashboard.get("panels", [])
    if len(panels) < 8 or len({panel.get("id") for panel in panels}) != len(panels):
        fail("the overview dashboard needs at least eight uniquely numbered panels")
    check_metric_references(json.dumps(dashboard), known, "Grafana dashboard")

    declared_alerts = set(policy.get("alerts", []))
    actual_alerts = set(ALERT_RE.findall(alerts_text))
    if actual_alerts != declared_alerts:
        fail(f"alert rules differ from policy; missing={sorted(declared_alerts - actual_alerts)}, extra={sorted(actual_alerts - declared_alerts)}")
    check_metric_references(alerts_text, known, "Prometheus rules")
    anchors = set(RUNBOOK_RE.findall(alerts_text))
    for anchor in anchors:
        if f'<a id="{anchor}"></a>' not in runbook_text:
            fail(f"alert runbook anchor is missing: {anchor}")

    semantics = capacity.get("fixed_semantics", {})
    realtime_source = REALTIME.read_text()
    if f"const MAX_ATTEMPTS: i64 = {semantics.get('claim_max_attempts')};" not in realtime_source:
        fail("A708 max-attempts value drifted from the provider")
    # The realtime tables are storage-module models (ADR 0038): every provider indexes
    # what the model declares.
    models = {model["kind"]: model for model in load(MODELS)["definitions"]}
    for index in semantics.get("required_indexes", []):
        model = models.get(index.get("model"), {})
        declared = model.get("range_indexes", []) + model.get("unique_indexes", [])
        if index.get("fields") not in declared:
            fail(f"A708 required index is not declared by its model: {index}")
    defaults = capacity.get("planning_defaults", {})
    if defaults.get("headroom_factor", 0) < 1.5 or defaults.get("storage_overhead_factor", 0) < 1:
        fail("A708 planning factors do not reserve usable headroom")
    admission = capacity.get("admission", {})
    if not admission.get("unmeasured_values_are_not_release_evidence"):
        fail("A708 must not allow planning defaults to masquerade as measurements")
    if len(admission.get("required_measurements", [])) < 6 or len(admission.get("rules", [])) < 5:
        fail("A708 lacks a measurable admission model")

    if "release freeze" not in runbook_text.lower() or "capacity worksheet" not in runbook_text.lower():
        fail("runbook lacks release-freeze or capacity procedure")
    print(f"observability policy holds ({len(metrics)} metrics, {len(slos)} SLOs, {len(actual_alerts)} alerts)")


if __name__ == "__main__":
    if len(sys.argv) > 2 or (len(sys.argv) == 2 and sys.argv[1] != "--check"):
        fail("usage: check_observability_policy.py [--check]")
    main()
