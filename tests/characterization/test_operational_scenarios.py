import json
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class OperationalScenarioTests(unittest.TestCase):
    def test_manifest_holds(self):
        subprocess.run(
            ["python3", "scripts/check_operational_scenarios.py", "--check"],
            cwd=ROOT,
            check=True,
        )

    def test_zero_tolerance_invariants_are_explicit(self):
        data = json.loads((ROOT / "contracts/testing/operational-scenarios-v1.json").read_text())
        thresholds = {
            key: value
            for scenario in data["scenarios"]
            for key, value in scenario["thresholds"].items()
        }
        for invariant in ("panics", "duplicate_effects", "unauthorized_acceptances", "unbalanced_journals"):
            self.assertEqual(thresholds[invariant], 0)


if __name__ == "__main__":
    unittest.main()
