import importlib.util
import json
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
PYTHON_CLIENT = ROOT / "apps/aseman-client/generated/public_v1.py"


def load_client():
    spec = importlib.util.spec_from_file_location("aseman_public_v1", PYTHON_CLIENT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


class PublicClientTests(unittest.TestCase):
    def test_generated_clients_are_fresh_and_complete(self):
        subprocess.run(
            ["python3", "scripts/generate_public_clients.py", "--check"],
            cwd=ROOT,
            check=True,
        )
        openapi = json.loads((ROOT / "contracts/public/openapi.json").read_text())
        module = load_client()
        self.assertEqual(len(openapi["paths"]), 76)
        self.assertEqual(len(module.OPERATIONS), len(openapi["paths"]))
        self.assertEqual(len(set(module.OPERATIONS)), len(module.OPERATIONS))

    def test_client_refuses_ambiguous_auth_before_network_io(self):
        module = load_client()
        client = module.AsemanClient("https://127.0.0.1", session="s", proof="p")
        with self.assertRaisesRegex(TypeError, "exactly one"):
            client.api_hello({})

    def test_client_refuses_mutation_without_idempotency_key(self):
        module = load_client()
        mutation = next(name for name, item in module.OPERATIONS.items() if item["mutation"])
        client = module.AsemanClient("https://127.0.0.1", session="s")
        with self.assertRaisesRegex(TypeError, "idempotency_key"):
            getattr(client, mutation)({})


if __name__ == "__main__":
    unittest.main()
