---
status: GENERATED
owner: module-platform
source_of_truth: contracts/module and scripts/generate_phase2_contracts.py
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_phase2_contracts.py --check
---

# Module contract and capability catalog

Protocol major: `1`.

## Module kinds

- `storage`
- `client_network`
- `federation`
- `security`
- `realtime`
- `finance_ledger`
- `consensus`
- `coordination`
- `vmm`
- `vmm_backend`
- `runtime`
- `telemetry`
- `sample`

## Sample provider capabilities

- `sample.echo`
- `sample.health`

## Contract inputs

| Path | SHA-256 |
|---|---|
| `contracts/module/admin.openapi.yaml` | `sha256:fb438b20fd8bd7a20fc23b40897a95e9002cf961f02aa7b52954ae0de177091c` |
| `contracts/module/amod.schema.json` | `sha256:bdf3d19f31b6774129a7b4ac41c59101d3fc9fd133db43a00372fbdcd8d95a27` |
| `contracts/module/bootstrap/bootstrap.schema.json` | `sha256:a8589c08efd6b13e8f3f26fcd9356ab9d076e69e9a5fa6c2e647de461aa027dc` |
| `contracts/module/bootstrap/signed-bootstrap.schema.json` | `sha256:ed3bd0d1d1e18cec9f6a400a2c06805765bf9ad9b61a45e042039cfde90b44bc` |
| `contracts/module/control/v1/control.proto` | `sha256:5358721edc9309722de1c6797feabd3532be57895ae4b583f3422758342b71e1` |
| `contracts/module/lifecycle.schema.json` | `sha256:f8c266c3560b0d7f4c6d9009711fe05505b54d274ecc1f3d67c71bfd36a14e3f` |
| `contracts/module/module.schema.json` | `sha256:c9180af4c052f2aee954490dc3b047f3eaec6c0fdfc0648e5530637a7c67be58` |
| `contracts/module/permissions.schema.json` | `sha256:2369d0e4c2944764b08248c71b88b55f6f52c3ccc60fb1b512fc9fde3bf95718` |
| `contracts/module/placement.schema.json` | `sha256:7656b7c7d1cac05f6de096bf0ac131c516baefb554b2bf5b47dd081291167a5b` |
| `contracts/module/protocol-compatibility.json` | `sha256:77abd5f82e416c756c5f57d6cc391418615473237f92216ea5ed59377094c46f` |
| `contracts/module/provider/v1/provider.proto` | `sha256:e0f9999e4a87d068fbc8628c981e544f86bd6dbbf749a83899da8ca7e795f3b1` |
| `contracts/module/sample/v1/sample.proto` | `sha256:62bb606da601f53e9495bc91ac0b173c6d3459058160f83be55ae8992dd93468` |
| `contracts/module/trust.schema.json` | `sha256:1e6e1deaa7e989c826c1bb18da5422bfa8510d2799b3d96bb9d5d63f614f7171` |

All module RPCs use generated bindings and the required request semantics
listed in the JSON catalog.
