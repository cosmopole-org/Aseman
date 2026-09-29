import importlib.util
import pathlib
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]


def load_checker():
    path = ROOT / "scripts/check_release_policy.py"
    spec = importlib.util.spec_from_file_location("check_release_policy", path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


class ReleasePolicyTests(unittest.TestCase):
    def test_repository_policy_and_workflow_hold(self) -> None:
        checker = load_checker()
        policy = checker.load_json(checker.POLICY_PATH)
        self.assertEqual(checker.check_repository(policy), [])

    def test_bundle_check_fails_closed_when_evidence_is_absent(self) -> None:
        checker = load_checker()
        policy = checker.load_json(checker.POLICY_PATH)
        with tempfile.TemporaryDirectory() as directory:
            problems = checker.check_bundle(policy, pathlib.Path(directory))
        self.assertTrue(any("SHA256SUMS" in problem for problem in problems))
        self.assertTrue(any("amd64" in problem for problem in problems))


if __name__ == "__main__":
    unittest.main()
