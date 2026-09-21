---
status: GENERATED
owner: storage/application
source_of_truth: contracts/capsule and scripts/generate_phase3_contracts.py
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_phase3_contracts.py --check
---

# Capsule contract and core-kind catalog

Encoding: `deterministic-cbor-v1`; integrity: `sha2-256`.

| Kind | Native table | Class | Consistency | Owner |
|---|---|---|---|---|
| `core.user` | `users` | `core` | `serializable` | `global` |
| `core.creature` | `creatures` | `core` | `serializable` | `global` |
| `core.program` | `programs` | `core` | `serializable` | `creature` |
| `core.store` | `stores` | `core` | `snapshot` | `creature` |
| `core.store_membership` | `store_memberships` | `core` | `serializable` | `creature` |
| `core.access_level` | `access_levels` | `core` | `serializable` | `creature` |
| `core.capability_grant` | `capability_grants` | `core` | `serializable` | `creature` |
| `core.session` | `sessions` | `core` | `serializable` | `global` |
| `core.file` | `files` | `core` | `snapshot` | `creature` |
| `core.workload` | `workloads` | `core` | `serializable` | `creature` |
| `core.workload_operation` | `workload_operations` | `core` | `serializable` | `creature` |
| `core.node` | `nodes` | `core` | `serializable` | `global` |
| `core.federation_peer` | `federation_peers` | `core` | `serializable` | `global` |
| `core.node_key` | `node_keys` | `core` | `serializable` | `node` |
| `finance.wallet` | `wallets` | `finance` | `serializable` | `creature` |
| `finance.ledger_entry` | `ledger_entries` | `finance` | `serializable` | `creature` |
| `finance.pricing_policy` | `pricing_policies` | `finance` | `serializable` | `global` |
| `finance.usage_record` | `usage_records` | `finance` | `serializable` | `creature` |
| `core.module_installation` | `module_installations` | `core` | `serializable` | `node` |
| `core.guest_database_binding` | `guest_database_bindings` | `core` | `serializable` | `creature` |
| `core.guest_schema_definition` | `guest_schema_definitions` | `core` | `serializable` | `creature` |
| `core.chain` | `chains` | `core` | `serializable` | `creature` |
| `core.chain_shard` | `chain_shards` | `core` | `serializable` | `creature` |
| `core.entity` | `entities` | `core` | `snapshot` | `creature` |
| `core.user_metadata` | `user_metadata_documents` | `core` | `serializable` | `global` |
| `core.creature_metadata` | `creature_metadata_documents` | `core` | `serializable` | `global` |
| `core.store_metadata` | `store_metadata_documents` | `core` | `serializable` | `creature` |
| `core.program_metadata` | `program_metadata_documents` | `core` | `serializable` | `creature` |
| `finance.legacy_record` | `legacy_finance_records` | `finance` | `serializable` | `global` |
| `core.creature_type` | `creature_types` | `core` | `serializable` | `global` |
| `core.gateway_route` | `gateway_routes` | `core` | `serializable` | `creature` |
| `core.program_alarm` | `program_alarms` | `core` | `serializable` | `creature` |
| `core.vm_resource_store` | `vm_resource_stores` | `core` | `serializable` | `creature` |
| `core.vm_resource_entity` | `vm_resource_entities` | `core` | `serializable` | `creature` |
| `core.entity_config` | `entity_configs` | `core` | `serializable` | `creature` |
| `core.entity_artifact` | `entity_artifacts` | `core` | `serializable` | `creature` |
| `core.creature_secret` | `creature_secrets` | `core` | `serializable` | `creature` |
| `core.secret_grant` | `secret_grants` | `core` | `serializable` | `creature` |
| `core.bridge_grant` | `bridge_grants` | `core` | `serializable` | `creature` |
| `core.bridge_topic` | `bridge_topics` | `core` | `serializable` | `creature` |

## Contract inputs

| Path | SHA-256 |
|---|---|
| `contracts/capsule/capabilities.schema.json` | `sha256:c5e2c86599a100654b1fd33759823e53d9744a515106832e29de667d6f331cb8` |
| `contracts/capsule/capsule.schema.json` | `sha256:c28fbc59a79f064edd8eae3c0d3903a3d549b752a19d2b7462dc5804b4a65e49` |
| `contracts/capsule/definition.schema.json` | `sha256:65c7fe6656c9795f0deeaef5eab76d5c42d84d036bf83006564745b7dca3ecfb` |
| `contracts/capsule/encoding.md` | `sha256:18fed00104ce6924115ca765a4d9f15ce21a766576ec7fcc0d9458281c76e547` |
| `contracts/capsule/fixtures/canonical-v1.json` | `sha256:d77cc7adf9cd135291179b3ad1160cff10b4718e7724ab63e5edbacd0522dbca` |
| `contracts/capsule/fixtures/compatible-core-capabilities.json` | `sha256:b82326ae4dad8bdcc1f8f16da8d195b76f66cb092f93709fed653f590dfa1be7` |
| `contracts/capsule/fixtures/incompatible-eventual-capabilities.json` | `sha256:5ee58a857c3074f3fc01e54512773d6879f4794ee17d1ba08a8a85a39be96801` |
| `contracts/capsule/fixtures/invalid-cbor-v1.json` | `sha256:59b209ba3e1fd144f417a6ca71137d290f64fb0859225159e1e7404b52d63e0f` |
| `contracts/capsule/guest/binding.schema.json` | `sha256:6f44a87548557051999567eae0049efb67b4fba29004f69e8a0e1a8293f39515` |
| `contracts/capsule/guest/fixtures/invalid-caller-routing.json` | `sha256:b48d583e43a34d0b5f06897c0aa6769442d665955759679bcb1a4757b1ede051` |
| `contracts/capsule/guest/fixtures/valid-multi-table.json` | `sha256:f2e19c02e568e81a0d71f78cc3f893e42691cfab52f5073d85183dc4912ea365` |
| `contracts/capsule/guest/isolation-rules.json` | `sha256:e15bfcf11512f7089651362eb6c3452cd8234930006a7aafc0b7b95590b832ec` |
| `contracts/capsule/guest/legacy-kv-table.json` | `sha256:7d2a86b730d74d34b6e30a4fe2a4394f9fc18fdfa14329f4c63c39c92fa7a079` |
| `contracts/capsule/guest/schema-command.schema.json` | `sha256:e5d44dd58dcc6ae69bc5263d64b4e647f0083eb264c659bb89506f5fedb40f36` |
| `contracts/capsule/guest/schema-mutation.schema.json` | `sha256:79cd855b2b4af54ba37345cdcc09c40325cfe2bd0a7e58028e85d8a728ba56fb` |
| `contracts/capsule/kinds/core-logical-schemas.json` | `sha256:4ab858ca47bb205dd7868f654419da010ac6ab52d0b51a791aa3132ed2ac1b4d` |
| `contracts/capsule/kinds/core-logical-schemas.schema.json` | `sha256:f9761ccb6a3b07bdcd15dbc7f3b76128345b63cd6606a38c8802feb4ca8e4c7e` |
| `contracts/capsule/kinds/core-registry.json` | `sha256:e9922ed1e0adb04bed15f1d62a601b07f6219dc181f02b8b7662a7e525947d06` |
| `contracts/capsule/kinds/core-registry.schema.json` | `sha256:3c6f29d9086fa764910bc02dda2866aada44cdccfd838f511fab1fbb24c085e8` |
| `contracts/capsule/kinds/storage-class-logical-schemas.json` | `sha256:7ec3875517611d016fc014b78ecd22069be10a1a3803400b487efe9d7abcd4e1` |
| `contracts/capsule/kinds/storage-class-logical-schemas.schema.json` | `sha256:2c3a7e3e9a8d92addeb090bcf3aa487c344a3535c8f8e9f8d4804b4b6fd10cef` |
| `contracts/capsule/kinds/storage-class-registry.json` | `sha256:1bc523adbf58aaa7cf4a4b89a7b8c4667f33bdbeb880f695154d4d3e9ea79787` |
| `contracts/capsule/kinds/storage-class-registry.schema.json` | `sha256:380038c8213588416846a566bd3ba120b6100ca173a45fb6948ed8cdde01e59e` |
| `contracts/capsule/provider/v1/protocol-compatibility.json` | `sha256:56bc068fa9a26dc28438a6861ec0036f97eca5534a3dbea1f27a48b9c7cb0603` |
| `contracts/capsule/provider/v1/storage.proto` | `sha256:614cd75d3972303c79785300692b11ba1a4dee5a742df56f6aa851f16838a85a` |
| `contracts/capsule/query/errors.schema.json` | `sha256:491728865aa3abd21626358268e04ff1bd84cbb219c9ddbda1047319effce7b6` |
| `contracts/capsule/query/fixtures/errors.json` | `sha256:7a1b9d0b63f525dbbcd9c9e3fbc3a51b8871bad79561aa82b3fe0d545f80c0d4` |
| `contracts/capsule/query/fixtures/invalid-raw-provider-query.json` | `sha256:8c0ab663dff2d491130175a0ad45923a572f7d4bd56bfecf6e3f42956efe7d36` |
| `contracts/capsule/query/fixtures/valid-bounded-query.json` | `sha256:f69e2f2ed9ebf359a615463750470b9ee23586bab7ba029cd7eafaf43199adf8` |
| `contracts/capsule/query/query.schema.json` | `sha256:4ff5d09d5b8a28a97df03374ba9f1928a96f5134be2d30c3b9d850fd470d358b` |
| `contracts/capsule/storage-class-semantics.json` | `sha256:bdad80b2878f3aea6f248c932a09ce287f7cc94ead03a99232ad54511e3afa3c` |

P3-01 is accepted; providers must still pass the A310 behavioral conformance kit.
