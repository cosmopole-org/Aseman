"""Drift checks for the observed legacy surface golden fixture."""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

import generate_characterization_fixtures as fixture_generator  # noqa: E402


FIXTURE_PATH = ROOT / "tests/characterization/surface-golden.json"


class SurfaceGoldenTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.actual = json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))
        cls.expected = fixture_generator.build_fixture()

    def test_fixture_matches_source_derived_inventories(self) -> None:
        self.assertEqual(self.expected, self.actual)

    def test_shell_actions_and_call_paths_are_one_to_one(self) -> None:
        routes = {
            row["path"] for row in self.actual["interfaces"]["signed_shell_actions"]
        }
        call_paths = {row["path"] for row in self.actual["action_call_paths"]}
        self.assertEqual(routes, call_paths)
        self.assertEqual(77, len(routes))

    def test_observed_dispatch_names_are_unique(self) -> None:
        interfaces = self.actual["interfaces"]
        guest_operations = [row["operation"] for row in interfaces["guest_operations"]]
        http_routes = [
            (row["method"], row["path"], row["surface"])
            for row in interfaces["http_routes"]
        ]
        runtime_keys = [row["key"] for row in self.actual["runtimes"]["providers"]]

        self.assertEqual(len(guest_operations), len(set(guest_operations)))
        self.assertEqual(len(http_routes), len(set(http_routes)))
        self.assertEqual(len(runtime_keys), len(set(runtime_keys)))
        self.assertEqual(111, len(guest_operations))
        self.assertEqual(31, len(http_routes))
        self.assertEqual(7, len(runtime_keys))

    def test_persistence_families_remain_accounted_for(self) -> None:
        persistence = self.actual["persistence"]
        object_types = {row["object_type"] for row in persistence["core_objects"]}
        table_names = {row["name"] for row in persistence["questdb_tables"]}
        cluster_families = set(persistence["cluster_rocksdb"]["column_families"])

        self.assertEqual(
            {"Chain", "ChainShard", "Creature", "Entity", "File", "Program", "Session", "Store"},
            object_types,
        )
        self.assertEqual({"buildlogs", "storage"}, table_names)
        self.assertEqual({"logs", "meta", "sm"}, cluster_families)

    def test_fixture_links_reviewed_support_acceptance(self) -> None:
        meta = self.actual["_meta"]
        self.assertEqual("ACCEPTED", meta["status"])
        self.assertEqual("VERIFIED", meta["lifecycle_status"])
        self.assertEqual(
            "tests/characterization/support-manifest.json",
            meta["support_acceptance"],
        )

    def test_every_observed_operation_has_support_ownership_and_evidence(self) -> None:
        manifest = json.loads(
            (ROOT / "tests/characterization/support-manifest.json").read_text(
                encoding="utf-8"
            )
        )
        expected_counts = {
            "signed_shell_actions": len(self.actual["interfaces"]["signed_shell_actions"]),
            "http_routes": len(self.actual["interfaces"]["http_routes"]),
            "guest_operations": len(self.actual["interfaces"]["guest_operations"]),
            "runtimes": len(self.actual["runtimes"]["providers"]),
        }
        for group, count in expected_counts.items():
            rows = manifest[group]
            self.assertEqual(count, len(rows), group)
            self.assertEqual(len(rows), len({row["id"] for row in rows}), group)
            for row in rows:
                self.assertTrue(row["current_owner"])
                self.assertTrue(row["target_owner"])
                self.assertTrue(row["characterization"])
                self.assertIn(row["support"], {"supported-compatibility", "remove"})


if __name__ == "__main__":
    unittest.main()
