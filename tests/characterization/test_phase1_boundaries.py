import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
CLIENT = ROOT / "node/src/drivers/network/client"


class PhaseOneBoundaryTests(unittest.TestCase):
    def test_tcp_and_websocket_share_one_session_orchestrator(self) -> None:
        session = (CLIENT / "session.rs").read_text()
        self.assertIn("pub(super) fn process_inbound", session)
        self.assertIn("auth_with_signature", session)
        self.assertIn("fetch_secure_action", session)

        for transport in ("tcp.rs", "ws.rs"):
            source = (CLIENT / transport).read_text()
            self.assertEqual(source.count("session::process_inbound"), 1, transport)
            self.assertNotIn("auth_with_signature", source, transport)
            self.assertNotIn("fetch_secure_action", source, transport)
            self.assertIn("impl SessionSocket for Socket", source, transport)
            self.assertIn("impl SessionTransport<Socket>", source, transport)

    def test_node_composition_uses_typed_configuration(self) -> None:
        composition = (ROOT / "node/src/lib.rs").read_text()
        config = (ROOT / "crates/aseman-config/src/lib.rs").read_text()
        self.assertIn("AsemanConfig::from_process_with_dotenv", composition)
        self.assertNotIn("env::var(", composition)
        self.assertNotIn("std::env::var(", composition)
        self.assertIn("pub struct NetworkConfig", config)
        self.assertIn("pub struct LegacyStorageConfig", config)

        for path in (ROOT / "node/src").rglob("*.rs"):
            source = path.read_text()
            self.assertNotIn("env::var(", source, str(path.relative_to(ROOT)))
            self.assertNotIn("env::var_os(", source, str(path.relative_to(ROOT)))

    def test_process_environment_reads_are_owned_by_config(self) -> None:
        owner = ROOT / "crates/aseman-config/src/lib.rs"
        for path in ROOT.rglob("*.rs"):
            if "target" in path.parts or path == owner:
                continue
            source = path.read_text()
            self.assertNotIn("env::var(", source, str(path.relative_to(ROOT)))
            self.assertNotIn("env::var_os(", source, str(path.relative_to(ROOT)))

    def test_oversized_creature_owner_is_partitioned_by_action_family(self) -> None:
        parent_path = ROOT / "node/src/shell/api/actions/creature.rs"
        finance_path = ROOT / "node/src/shell/api/actions/creature/finance.rs"
        parent = parent_path.read_text()
        finance = finance_path.read_text()

        self.assertLess(len(parent.splitlines()), 2_000)
        self.assertIn("mod finance;", parent)
        self.assertIn("finance::handlers", parent)
        self.assertNotIn("fn payment_adjustment", parent)
        self.assertIn("fn payment_adjustment", finance)
        self.assertIn("pub(super) fn handlers", finance)


if __name__ == "__main__":
    unittest.main()
