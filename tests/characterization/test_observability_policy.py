import json
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class ObservabilityPolicyTests(unittest.TestCase):
    def test_policy_and_deployment_assets_are_consistent(self):
        subprocess.run(
            ["python3", "scripts/check_observability_policy.py", "--check"],
            cwd=ROOT,
            check=True,
        )

    def test_capacity_admission_needs_measurements_not_defaults(self):
        capacity = json.loads((ROOT / "contracts/realtime/capacity-v1.json").read_text())
        self.assertTrue(capacity["admission"]["unmeasured_values_are_not_release_evidence"])
        measurements = set(capacity["admission"]["required_measurements"])
        self.assertIn("measured_sustained_publish_events_per_second", measurements)
        self.assertIn("postgres_available_bytes", measurements)

    def test_metric_labels_exclude_identity_cardinality(self):
        policy = json.loads((ROOT / "contracts/observability/policy-v1.json").read_text())
        forbidden = set(policy["labels"]["forbidden_high_cardinality"])
        for metric in policy["metrics"]:
            self.assertTrue(forbidden.isdisjoint(metric["labels"]), metric["name"])


if __name__ == "__main__":
    unittest.main()
