import json
import pathlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
CAPSULE = ROOT / "contracts/capsule"


class PhaseThreeCapsuleContractTests(unittest.TestCase):
    def test_capsule_schemas_are_closed_and_parseable(self) -> None:
        schemas = list(CAPSULE.rglob("*.schema.json"))
        self.assertGreaterEqual(len(schemas), 5)
        for path in schemas:
            schema = json.loads(path.read_text())
            self.assertEqual(schema["$schema"], "https://json-schema.org/draft/2020-12/schema")
            self.assertTrue(
                schema.get("additionalProperties") is False
                or schema.get("unevaluatedProperties") is False,
                path,
            )

    def test_core_registry_has_unique_native_tables_and_no_universal_guest_table(self) -> None:
        registry = json.loads((CAPSULE / "kinds/core-registry.json").read_text())
        kinds = [row["kind"] for row in registry["kinds"]]
        tables = [row["table"] for row in registry["kinds"]]
        self.assertEqual(len(kinds), len(set(kinds)))
        self.assertEqual(len(tables), len(set(tables)))
        self.assertGreaterEqual(len(kinds), 20)
        self.assertNotIn("guest_capsules", tables)

        logical = json.loads((CAPSULE / "kinds/core-logical-schemas.json").read_text())
        definitions = {row["kind"]: row for row in logical["definitions"]}
        self.assertEqual(set(kinds), set(definitions))
        for kind, definition in definitions.items():
            declared = set(definition["fields"]) | set(definition["relationships"])
            self.assertTrue(set(definition["required"]) <= set(definition["fields"]), kind)
            for index in definition["unique_indexes"]:
                self.assertTrue(set(index) <= declared, (kind, index))
            for target in definition["relationships"].values():
                self.assertIn(target, definitions, (kind, target))

    def test_canonical_vector_is_versioned_and_complete(self) -> None:
        vector = json.loads((CAPSULE / "fixtures/canonical-v1.json").read_text())
        self.assertEqual(vector["encoding_version"], 1)
        self.assertEqual(vector["digest_algorithm"], "sha2-256")
        self.assertEqual(len(vector["integrity_hex"]), 64)
        self.assertGreater(len(vector["canonical_hex"]), 200)

        invalid = json.loads((CAPSULE / "fixtures/invalid-cbor-v1.json").read_text())
        self.assertGreaterEqual(len(invalid), 5)
        self.assertEqual(len({row["name"] for row in invalid}), len(invalid))

    def test_query_and_capability_examples_freeze_provider_independent_values(self) -> None:
        valid = json.loads(
            (CAPSULE / "query/fixtures/valid-bounded-query.json").read_text()
        )
        invalid = json.loads(
            (CAPSULE / "query/fixtures/invalid-raw-provider-query.json").read_text()
        )
        schema = json.loads((CAPSULE / "query/query.schema.json").read_text())
        self.assertTrue(set(valid) <= set(schema["properties"]))
        self.assertIn("raw_sql", invalid)
        self.assertNotIn("raw_sql", schema["properties"])
        self.assertFalse(schema["additionalProperties"])

        errors = json.loads((CAPSULE / "query/fixtures/errors.json").read_text())
        codes = {row["code"] for row in errors}
        self.assertIn("invalid_query", codes)
        self.assertIn("revision_conflict", codes)
        self.assertIn("unsupported_capability", codes)

        compatible = json.loads(
            (CAPSULE / "fixtures/compatible-core-capabilities.json").read_text()
        )
        incompatible = json.loads(
            (CAPSULE / "fixtures/incompatible-eventual-capabilities.json").read_text()
        )
        self.assertIn("consistency.linearizable", compatible["capabilities"])
        self.assertNotIn("consistency.linearizable", incompatible["capabilities"])
        self.assertTrue(all("." in capability for capability in compatible["capabilities"]))

    def test_storage_rpc_surface_matches_compatibility_fixture(self) -> None:
        provider = CAPSULE / "provider/v1"
        fixture = json.loads((provider / "protocol-compatibility.json").read_text())
        proto = (provider / "storage.proto").read_text()
        for message, fields in fixture["messages"].items():
            found = re.search(
                rf"message\s+{re.escape(message)}\s*\{{(?P<body>.*?)\}}", proto, re.S
            )
            self.assertIsNotNone(found, message)
            body = found.group("body")
            for field, number in fields.items():
                self.assertRegex(body, rf"\b{re.escape(field)}\s*=\s*{number}\s*;")
        service = fixture["service"]
        found = re.search(
            rf"service\s+{re.escape(service['name'])}\s*\{{(?P<body>.*?)\}}",
            proto,
            re.S,
        )
        self.assertIsNotNone(found)
        self.assertEqual(
            re.findall(r"\brpc\s+(\w+)\s*\(", found.group("body")),
            service["methods"],
        )

    def test_postgres_mapping_is_native_complete_and_guest_payload_free(self) -> None:
        registry = json.loads((CAPSULE / "kinds/core-registry.json").read_text())
        core = {row["kind"]: row for row in registry["kinds"] if row["storage_class"] == "core"}
        mapping = json.loads(
            (ROOT / "contracts/storage/postgres/core-mapping.json").read_text()
        )
        mapped = {row["kind"]: row for row in mapping["tables"]}
        self.assertEqual(set(mapped), set(core))
        self.assertEqual(len({row["table"] for row in mapped.values()}), len(mapped))
        self.assertNotIn("guest_capsules", {row["table"] for row in mapped.values()})
        for kind, row in mapped.items():
            self.assertEqual(row["table"], core[kind]["table"])
            self.assertEqual(set(row["fields"]), set(row["field_columns"]))
            self.assertFalse(set(row["field_columns"].values()) & set(row["relationships"]))

        migration = (
            ROOT / "modules/storage/postgres/migrations/0001_core.sql"
        ).read_text()
        self.assertEqual(migration.count("CREATE TABLE IF NOT EXISTS"), len(core))
        self.assertIn("REVOKE ALL ON SCHEMA aseman_core FROM PUBLIC", migration)
        self.assertNotIn("JSONB", migration.upper())

    def test_guest_contract_forbids_caller_routing_and_shared_tenancy(self) -> None:
        guest = CAPSULE / "guest"
        rules = json.loads((guest / "isolation-rules.json").read_text())
        self.assertEqual(rules["routing"]["caller_selectable"], [])
        self.assertEqual(
            set(rules["routing"]["server_selected"]),
            {"provider_id", "database_name", "role_name", "generation"},
        )
        self.assertTrue(rules["postgres"]["database_per_creature"])
        self.assertTrue(rules["postgres"]["role_per_creature"])
        self.assertFalse(rules["postgres"]["role_login"])
        self.assertFalse(rules["guest_schema"]["shared_guest_capsules_table"])
        self.assertTrue(rules["guest_schema"]["multiple_tables"])
        self.assertTrue(rules["guest_schema"]["catalog_compare_and_swap"])

        auth = rules["signed_proxy_authentication"]
        self.assertEqual(auth["implementation_work_unit"], "P4-04")
        self.assertFalse(auth["workload_credentials"])
        self.assertFalse(auth["direct_provider_access"])
        self.assertIn("replay", auth["required_checks"])
        self.assertIn("guest_database_binding", auth["authoritative_resolution"])

        invalid = json.loads(
            (guest / "fixtures/invalid-caller-routing.json").read_text()
        )
        mutation = json.loads((guest / "schema-mutation.schema.json").read_text())
        allowed = set().union(
            *(set(branch["properties"]) for branch in mutation["oneOf"])
        )
        self.assertFalse(
            {"provider_id", "database_name", "role_name", "creature_id"} <= allowed
        )
        self.assertTrue(
            {"provider_id", "database_name", "role_name"} <= set(invalid)
        )

    def test_guest_schema_commands_are_revision_fenced_and_routing_free(self) -> None:
        schema = json.loads(
            (CAPSULE / "guest/schema-command.schema.json").read_text()
        )
        self.assertEqual(
            set(schema["required"]), {"expected_catalog_revision", "mutation"}
        )
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(
            schema["properties"]["mutation"]["$ref"],
            "schema-mutation.schema.json",
        )
        self.assertFalse(
            {"provider_id", "database_name", "role_name", "creature_id"}
            & set(schema["properties"])
        )

    def test_non_core_storage_classes_have_explicit_native_semantics(self) -> None:
        registry = json.loads(
            (CAPSULE / "kinds/storage-class-registry.json").read_text()
        )
        logical = json.loads(
            (CAPSULE / "kinds/storage-class-logical-schemas.json").read_text()
        )
        mapping = json.loads(
            (ROOT / "contracts/storage/postgres/storage-class-mapping.json").read_text()
        )
        registered = {row["kind"]: row for row in registry["kinds"]}
        definitions = {row["kind"]: row for row in logical["definitions"]}
        mapped = {row["kind"]: row for row in mapping["tables"]}
        self.assertEqual(set(registered), set(definitions))
        self.assertEqual(set(registered), set(mapped))
        self.assertEqual(
            {row["storage_class"] for row in registered.values()},
            {"telemetry", "audit", "finance", "outbox", "realtime"},
        )
        self.assertEqual(
            len({(row["schema"], row["table"]) for row in mapped.values()}),
            len(mapped),
        )
        for kind, row in mapped.items():
            self.assertEqual(row["consistency"], registered[kind]["consistency"])
            self.assertEqual(
                row["mutation_policy"], definitions[kind]["mutation_policy"]
            )
            self.assertEqual(set(row["fields"]), set(row["field_columns"]))

        semantics = json.loads(
            (CAPSULE / "storage-class-semantics.json").read_text()
        )
        self.assertFalse(semantics["universal_payload_table"])
        self.assertFalse(semantics["classes"]["finance"]["eventual_fallback"])
        self.assertFalse(semantics["classes"]["outbox"]["eventual_fallback"])
        self.assertTrue(semantics["classes"]["audit"]["append_only"])
        self.assertEqual(
            semantics["classes"]["realtime"]["delivery"], "at_least_once"
        )

    def test_finance_projection_and_generated_class_ddl_do_not_drift(self) -> None:
        core = json.loads((CAPSULE / "kinds/core-registry.json").read_text())
        classes = json.loads(
            (CAPSULE / "kinds/storage-class-registry.json").read_text()
        )
        core_finance = {
            row["kind"]: row
            for row in core["kinds"]
            if row["storage_class"] == "finance"
        }
        class_finance = {
            row["kind"]: row
            for row in classes["kinds"]
            if row["storage_class"] == "finance"
        }
        self.assertEqual(core_finance, class_finance)

        migration = (
            ROOT / "modules/storage/postgres/migrations/0002_storage_classes.sql"
        ).read_text()
        self.assertNotIn("JSONB", migration.upper())
        self.assertNotIn("guest_capsules", migration)
        self.assertEqual(
            migration.count("CREATE TABLE IF NOT EXISTS"), len(classes["kinds"])
        )
        definitions = json.loads(
            (CAPSULE / "kinds/storage-class-logical-schemas.json").read_text()
        )["definitions"]
        self.assertEqual(
            migration.count(
                "EXECUTE FUNCTION aseman_storage.reject_capsule_mutation()"
            ),
            sum(row["mutation_policy"] == "append_only" for row in definitions),
        )

    def test_legacy_transform_manifest_exhaustively_accounts_for_a004(self) -> None:
        source = json.loads(
            (ROOT / "docs/generated/current-storage-access.json").read_text()
        )
        manifest = json.loads(
            (ROOT / "contracts/migration/legacy-transform-manifest.json").read_text()
        )
        access_identity = lambda row: (
            row["source"],
            row["method"],
            row["mode"],
            row["logical_template"],
        )
        candidate_identity = lambda row: (
            row["source"],
            row["logical_template"],
            row["review_status"],
        )
        self.assertEqual(
            {access_identity(row) for row in source["application_accesses"]},
            {access_identity(row) for row in manifest["application_accesses"]},
        )
        self.assertEqual(
            {candidate_identity(row) for row in source["candidate_key_templates"]},
            {candidate_identity(row) for row in manifest["candidate_key_templates"]},
        )
        self.assertEqual(
            {row["object_type"] for row in source["core_objects"]},
            {row["object_type"] for row in manifest["core_objects"]},
        )
        self.assertEqual(
            {row["name"] for row in source["questdb_tables"]},
            {row["name"] for row in manifest["questdb_tables"]},
        )
        self.assertEqual(
            {row["family"] for row in source["hashgraph_rocksdb"]},
            {row["family"] for row in manifest["hashgraph_families"]},
        )
        self.assertEqual(manifest["unknown_record_policy"], "fail_closed")
        # A308 acceptance: every A004 row carries a reviewed, non-blocked disposition.
        self.assertEqual(manifest["_meta"]["status"], "ACCEPTED")
        self.assertEqual(manifest["summary"]["blocked_rows"], 0)
        rows = [
            *manifest["application_accesses"],
            *manifest["candidate_key_templates"],
            *manifest["core_objects"],
            *manifest["questdb_tables"],
            *manifest["hashgraph_families"],
            manifest["cluster_rocksdb"],
        ]
        self.assertTrue(all(row["disposition"] for row in rows))
        self.assertFalse([row for row in rows if row["disposition"].startswith("blocked")])

    def test_capsule_export_contract_binds_source_manifest_order_and_digest(self) -> None:
        schema = json.loads(
            (ROOT / "contracts/migration/capsule-export.schema.json").read_text()
        )
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(set(schema["required"]), {"header", "records", "trailer"})
        header = schema["$defs"]["header"]
        self.assertFalse(header["additionalProperties"])
        self.assertEqual(header["properties"]["format_version"]["const"], 1)
        self.assertIn("transform_manifest_digest", header["required"])
        self.assertIn("source_snapshot_id", header["required"])
        self.assertEqual(
            schema["$defs"]["digest"]["minItems"],
            schema["$defs"]["digest"]["maxItems"],
        )
        self.assertFalse(schema["$defs"]["checkpoint"]["additionalProperties"])


if __name__ == "__main__":
    unittest.main()
