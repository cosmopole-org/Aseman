import json
import pathlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
MODULE = ROOT / "contracts/module"


class PhaseTwoModuleContractTests(unittest.TestCase):
    def test_all_module_schemas_are_closed_and_parseable(self) -> None:
        schemas = list(MODULE.rglob("*.schema.json"))
        self.assertGreaterEqual(len(schemas), 5)
        for path in schemas:
            schema = json.loads(path.read_text())
            self.assertEqual(schema["$schema"], "https://json-schema.org/draft/2020-12/schema")
            self.assertFalse(schema.get("additionalProperties", True), path)

    def test_protobuf_field_numbers_match_the_compatibility_fixture(self) -> None:
        fixture = json.loads((MODULE / "protocol-compatibility.json").read_text())
        proto_text = "\n".join(path.read_text() for path in MODULE.rglob("*.proto"))
        for package in fixture["packages"].values():
            for message, fields in package.items():
                found = re.search(rf"message\s+{re.escape(message)}\s*\{{(?P<body>.*?)\}}", proto_text, re.S)
                self.assertIsNotNone(found, message)
                body = found.group("body")
                for field, number in fields.items():
                    self.assertRegex(body, rf"\b{re.escape(field)}\s*=\s*{number}\s*;")

    def test_control_contract_requires_operational_metadata(self) -> None:
        control = (MODULE / "control/v1/control.proto").read_text()
        for field in ("request_id", "trace_id", "deadline_unix_millis", "cancellation_id", "idempotency_key"):
            self.assertIn(field, control)
        for code in ("UNSUPPORTED", "DEADLINE_EXCEEDED", "CANCELLED", "UNAVAILABLE"):
            self.assertIn(code, control)

    def test_rpc_surface_matches_the_compatibility_fixture(self) -> None:
        fixture = json.loads((MODULE / "protocol-compatibility.json").read_text())
        proto_text = "\n".join(path.read_text() for path in MODULE.rglob("*.proto"))
        for service, methods in fixture["services"].items():
            found = re.search(rf"service\s+{re.escape(service)}\s*\{{(?P<body>.*?)\}}", proto_text, re.S)
            self.assertIsNotNone(found, service)
            body = found.group("body")
            actual = re.findall(r"\brpc\s+(\w+)\s*\(", body)
            self.assertEqual(actual, methods, service)

    def test_authenticated_admin_edge_is_composed(self) -> None:
        contract = (MODULE / "admin.openapi.yaml").read_text()
        cli = (ROOT / "apps/asemanctl/src/cli/modules.rs").read_text()
        server = (ROOT / "apps/aseman-node/src/drivers/cluster/server.rs").read_text()
        backend = (ROOT / "apps/aseman-node/src/drivers/module_admin.rs").read_text()
        self.assertIn("bearerAuth", contract)
        self.assertIn("artifactBase64", cli)
        self.assertIn("/v1/admin/modules", server)
        self.assertIn("requires a configured cluster auth token", server)
        self.assertIn("start_module_admin", server)
        self.assertIn("impl ModuleAdministration for ModuleAdminService", backend)


if __name__ == "__main__":
    unittest.main()
