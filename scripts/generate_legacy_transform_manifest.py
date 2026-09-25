#!/usr/bin/env python3
"""Account for every A004 legacy access without inventing unreviewed transforms."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "docs/generated/current-storage-access.json"
JSON_OUT = ROOT / "contracts/migration/legacy-transform-manifest.json"
MD_OUT = ROOT / "docs/generated/legacy-transform-manifest.md"
GENERATOR = "scripts/generate_legacy_transform_manifest.py"
OBJECT_TARGETS = {
    "Chain": "core.chain",
    "ChainShard": "core.chain_shard",
    "Creature": "core.creature",
    "Entity": "core.entity",
    "File": "core.file",
    "Program": "core.program",
    "Session": "core.session",
    "Store": "core.store",
}
# ADR 0016 reviewed JSON document families: legacy key prefix -> target kind.
# The root `metadata` record is the authority; `metadata.*` splats are rebuilt and compared.
DOCUMENT_TARGETS = {
    "UserMeta::": "core.user_metadata",
    "CreatMeta::": "core.creature_metadata",
    "StoreMeta::": "core.store_metadata",
    "ProgMeta::": "core.program_metadata",
}
# The generic `json::` layout writers in the legacy transaction are aggregated into the
# per-family document transforms above; unreviewed key families still fail closed.
JSON_LAYOUT_TEMPLATES = {"json::{}::{}", "json::{}::{}.{}"}
# Raw deletes of keys that no legacy writer produces. `del_key` takes a physical key, so
# `Json::StoreMeta::{id}::metadata` never matches `json::StoreMeta::{id}::metadata`; the
# delete is a no-op that leaves orphaned StoreMeta documents, which fail closed (ADR 0016).
NO_OP_DELETE_TEMPLATES = {"Json::StoreMeta::{}::metadata"}
# ADR 0017 legacy finance epoch (`finance.legacy_record`): JSON records, authoritative
# counters, and idempotency markers migrate; derived counters and listings are verified.
FINANCE_DOCUMENT_PREFIXES = (
    "Json::FinanceHold::", "Json::FinancePool::", "Json::FinancePoolReservation::",
    "Json::FinanceLiveDebit::", "Json::FinancePayout::", "Json::FinanceJournal::",
    "Json::FinanceProjectBudget::", "Json::BillingCatalog::", "Json::BillingQuote::",
    "Json::VmBilling::", "Json::Creature::", "Json::CreatureNamespace::billing",
    "Json::CreatureNamespace::market",
)
FINANCE_RECORD_LINKS = {
    "MintApplied", "FinanceDebt", "FinanceWithdrawable", "FinanceHoldRequest", "FinanceRun",
    "FinanceSettlement", "FinanceRelease", "FinancePayoutRequest", "FinancePayoutResolution",
    "FinancePoolOpen", "FinancePoolRefresh", "FinancePoolClose", "FinancePoolSettlement",
    "FinancePoolDebit", "PaymentAdjustment",
}
FINANCE_DERIVED_LINKS = {
    "FinanceHeld", "FinancePayoutHeld", "FinanceSpent", "FinanceEarned",
    "FinanceHoldByPayer", "FinanceJournalByUser", "FinancePayoutByUser", "FinancePoolByUser",
}
# Link/index families the export consumes or verifies against reviewed objects. Every
# other link family can hold primary state (balances, keys, VM fields) and is blocked.
VERIFIED_LINK_FAMILIES = {"creatorof", "UserIdToEmail", "UserEmailToId", "ownerof", "machinePrograms"}
VERIFIED_INDEX_FAMILIES = {("Creature", "username", "id"), ("Session", "userId", "id")}
# `Program` indexes are read and deleted but no legacy writer ever creates one.
UNWRITTEN_INDEX_FAMILIES = {"Program"}
DERIVED_METHODS = {
    "get_index", "put_index", "del_index", "has_index", "get_link", "put_link",
    "get_links_list", "search_link_vals_list", "search_link_keys_list_by_prefix",
}


def classify_access(row: dict[str, object]) -> dict[str, object]:
    template = row["logical_template"]
    method = row["method"]
    if method in DERIVED_METHODS or template.startswith(("link::", "index::")):
        return {**row, **classify_link_or_index(template, method, row["source"])}
    elif template in OBJECT_TARGETS:
        status = "aggregate_with_object_family"
        target = OBJECT_TARGETS[template]
        note = "deduplicate with the object-column aggregate; never emit per access"
    elif template.startswith("obj::"):
        status = "aggregate_with_object_family"
        target = next(
            (kind for name, kind in OBJECT_TARGETS.items() if template.startswith(f"obj::{name}::")),
            None,
        )
        note = "object column must be assembled and transformed once"
    elif any(template.startswith(prefix) for prefix in DOCUMENT_TARGETS):
        status = "fixture_backed_transform"
        target = next(kind for prefix, kind in DOCUMENT_TARGETS.items() if template.startswith(prefix))
        note = "ADR 0016 document capsule; subject resolved server-side; stale splats fail closed"
    elif template.startswith("Json::CreatureType::"):
        status = "fixture_backed_transform"
        target = "core.creature_type"
        note = "ADR 0016 global creature type spec document keyed by type name"
    elif template.startswith(FINANCE_DOCUMENT_PREFIXES):
        status = "fixture_backed_transform"
        target = "finance.legacy_record"
        note = "ADR 0017 finance epoch record; legacy reconciliation invariants fail closed"
    elif template in NO_OP_DELETE_TEMPLATES and method == "del_key":
        status = "reviewed_no_persisted_record"
        target = None
        note = "no writer produces this physical key; orphaned StoreMeta documents fail closed"
    elif template in JSON_LAYOUT_TEMPLATES:
        status = "aggregate_with_document_family"
        target = None
        note = "legacy JSON layout writer; reviewed families aggregate per key, others fail closed"
    elif template == "globalIdCounter":
        status = "intentional_removal"
        target = None
        note = "ADR 0020 ID counter verified against minted IDs; UUIDv7 replaces it"
    elif template.startswith("god::"):
        status = "reviewed_no_persisted_record"
        target = None
        note = "ADR 0020 superuser flag has no writer; any record fails closed for review"
    elif row["source"].startswith("apps/aseman-node/src/core/core_orchestrator.rs") and (
        "|" in template or template.endswith(("::targetCount", "::tempCount"))
    ):
        status = "intentional_removal"
        target = None
        note = "ADR 0020 dead write-only chainCallback state; shape-checked, never migrated"
    elif method == "get_by_prefix" and template == "{}::{}" and row["source"].startswith("modules/runtime/"):
        status = "reviewed_no_persisted_record"
        target = None
        note = "ADR 0021 guest getByPrefix scans raw keys; committed guest pairs are link records"
    elif method == "del_key" and template in {"{}::payload", "{}::meta"}:
        status = "reviewed_no_persisted_record"
        target = None
        note = "raw delete of an unprefixed Json::VmResourceEntity key never matches a record"
    elif method == "del_key" and template.startswith("Json::VmResourceStore::"):
        status = "reviewed_no_persisted_record"
        target = None
        note = "raw delete of an unprefixed Json::VmResourceStore key never matches a record (ADR 0022)"
    elif (vm := vm_disposition(candidate_family(template))) is not None:
        status, target, note = vm["disposition"], vm["target_kind"], vm["note"]
    elif template.startswith("json::") or template.startswith("Json::") or "Meta::" in template:
        status = "blocked_payload_fixture"
        target = None
        note = "JSON shape and owner must be proven by fixtures before transformation"
    else:
        status = "blocked_semantic_review"
        target = None
        note = "caller ownership, retention, and target meaning are not yet proven"
    return {**row, "disposition": status, "target_kind": target, "note": note}


# Candidate families already covered by a reviewed decision. Candidates are heuristic
# `format!` matches, so a covered family points at its decision instead of re-reviewing.
COVERED_CANDIDATE_FAMILIES = {
    **{name: "ADR 0016 metadata documents" for name in ("UserMeta", "CreatMeta", "StoreMeta", "ProgMeta")},
    "Json::CreatureType": "ADR 0016 creature type registry",
    "CreatureTypeExists": "ADR 0016 creature type registry",
    **{name: "ADR 0017 finance epoch" for name in FINANCE_RECORD_LINKS | FINANCE_DERIVED_LINKS},
    **{prefix.removesuffix("::"): "ADR 0017 finance epoch" for prefix in FINANCE_DOCUMENT_PREFIXES},
    "hasaccess": "ADR 0018 store memberships",
    "onaccess": "ADR 0018 store memberships",
    "UserPrivateKey": "ADR 0019 custodial keys (intentional removal)",
    **{name: "verified link family" for name in VERIFIED_LINK_FAMILIES},
    "Json::StoreMeta": "no-op legacy delete (reviewed)",
    "obj": "typed object layout",
    "index": "verified index layout",
    "chainCallback": "ADR 0020 dead chain-callback state",
    "{}|{}": "ADR 0020 dead chain-callback state",
    "god": "ADR 0020 superuser flag (fails closed)",
    "AppletDb": "ADR 0021 guest KV (applet_db namespace)",
    "Temp": "read-only consumedTokens flag with no legacy writer",
}
# ADR 0022 VM state: observed runtime stays with the VMM; durable intent migrates.
VM_OBSERVED = {
    "VmInstance", "VmStatus", "VmStartedAt", "VmOwnerProgram", "vmDistributed",
    "VmContainerName", "vmStandaloneImageName", "vmStandaloneContainerName", "VmTerminal",
    "VmBuilds", "ProxyCorrExpiry", "Json::ProxyCorrelation", "ModalApp", "ModalImage",
    "ModalVolume", "ModalSandbox", "ModalProvisioning", "ModalProvisioningError",
}
VM_INTENT = {
    "vmHttpRoute": "core.gateway_route",
    "vmAlarmStoreId": "core.program_alarm", "vmAlarmTime": "core.program_alarm",
    "vmAlarmData": "core.program_alarm", "vmAlarmEntity": "core.program_alarm",
    "Json::VmResourceStore": "core.vm_resource_store",
    "Json::VmResourceEntity": "core.vm_resource_entity",
    "Json::ProxyEntity": "core.entity_config",
    "vmEntityPath": "core.entity_artifact", "vmEntityDownloadable": "core.entity_artifact",
}
VM_DERIVED = {"vmHttpRouteFor", "vmHttpRouteUser", "vmOwnedStore", "vmEntityType", "VmBilling"}
VM_REMOVED = {"vmDistribution"}


# ADR 0023 secrets/login grants and ADR 0024 bridge state.
SECURITY_DISPOSITIONS = {
    "Secret": ("fixture_backed_transform", "core.creature_secret", "ADR 0023 authenticated ciphertext; plaintext never exported"),
    "{SECRET_PREFIX}{owner}": ("fixture_backed_transform", "core.creature_secret", "ADR 0023 authenticated ciphertext"),
    "SecretGrant": ("fixture_backed_transform", "core.secret_grant", "ADR 0023 grant with verified reverse link"),
    "{SECRET_GRANT_PREFIX}{owner}": ("fixture_backed_transform", "core.secret_grant", "ADR 0023 grant"),
    "SecretGrantee": ("derived_index_or_relationship", None, "ADR 0023 reverse index must mirror its grant"),
    "{SECRET_GRANTEE_PREFIX}{grantee}": ("derived_index_or_relationship", None, "ADR 0023 reverse index"),
    "LoginGrant": ("intentional_removal", None, "ADR 0023 ephemeral bearer nonce; shape-checked, never migrated"),
    "Json::BridgeGrant": ("fixture_backed_transform", "core.bridge_grant", "ADR 0024 digest-keyed bridge grant"),
    "BridgeTopicOwner": ("fixture_backed_transform", "core.bridge_topic", "ADR 0024 topic claim"),
    "NodeIpToHost": ("reviewed_no_persisted_record", None, "ADR 0024 no legacy writer; any record fails closed"),
}


def vm_disposition(family: str) -> dict[str, object] | None:
    if family in SECURITY_DISPOSITIONS:
        disposition, target, note = SECURITY_DISPOSITIONS[family]
        return {"disposition": disposition, "target_kind": target, "note": note}
    if family in VM_OBSERVED:
        return {"disposition": "vmm_observed_runtime", "target_kind": None,
                "note": "ADR 0022 VMM-owned observed runtime; handoff inventory; rebuilt by P5 reconciliation (RL-013)"}
    if family in VM_INTENT:
        return {"disposition": "fixture_backed_transform", "target_kind": VM_INTENT[family],
                "note": "ADR 0022 durable VM intent"}
    if family in VM_DERIVED:
        return {"disposition": "derived_index_or_relationship", "target_kind": None,
                "note": "ADR 0022 derived link verified against its record"}
    if family in VM_REMOVED:
        return {"disposition": "intentional_removal", "target_kind": None,
                "note": "ADR 0012/0022 OpenRaft replication scope; placement belongs to the P6 scheduler"}
    return None


# Owners for families that stay blocked until their phase reviews them.
OWNED_BLOCKED_PREFIXES: dict[str, str] = {}
# Unreviewed families with a named owning phase; they stay blocked but are not orphans.


# Per-source review of generic `{}::{}` style candidates (template families carry no name).
SOURCE_REVIEWS = {
    "apps/aseman-node/src/core/globe.rs": ("not_a_storage_key", "hash/seed input, never persisted"),
    "modules/consensus/hashgraph/src/net/net_transport.rs": ("not_a_storage_key", "in-memory RPC channel map"),
    "apps/aseman-node/src/drivers/network/federation/netserver.rs": ("not_a_storage_key", "routing target label"),
    "apps/aseman-node/src/drivers/vmm/network/gateway_registry.rs": ("not_a_storage_key", "process-local gateway map"),
    "apps/aseman-node/src/drivers/vmm/network/gateway_types.rs": ("not_a_storage_key", "process-local gateway map key"),
    "modules/runtime/elpify/src/queue.rs": ("not_a_storage_key", "process-local VM queue map"),
    "modules/runtime/javascript/src/runtime.rs": ("not_a_storage_key", "process-local VM map"),
    "modules/runtime/wasm/src/runtime.rs": ("not_a_storage_key", "process-local VM map"),
    "modules/runtime/javascript/src/host_calls.rs": ("covered_by_reviewed_family", "ADR 0021 guest dbop namespace"),
    "modules/runtime/wasm/src/host_calls.rs": ("covered_by_reviewed_family", "ADR 0021 guest dbop namespace"),
    "apps/aseman-node/src/drivers/vmm/host/vm_host_functions.rs": ("covered_by_reviewed_family", "ADR 0021 applet_db prefix composition"),
    "apps/aseman-node/src/core/actor/model/trx.rs": ("covered_by_reviewed_family", "json/link physical layout internals"),
    "apps/aseman-node/src/core/core_orchestrator.rs": ("covered_by_reviewed_family", "ADR 0020 dead chain-callback state"),
    "apps/aseman-node/src/shell/api/model/entity.rs": ("covered_by_reviewed_family", "typed Entity composite identity"),
    "apps/aseman-node/src/drivers/vmm/host/functions/login_grant.rs": ("covered_by_reviewed_family", "ADR 0023 login grant delete path"),
}
for _source in (
    "apps/aseman-node/src/drivers/vmm/hostcall_logs.rs", "apps/aseman-node/src/shell/api/actions/program.rs",
    "apps/aseman-node/src/drivers/vmm/host/functions/vm_ownership.rs", "apps/aseman-node/src/drivers/vmm/host_bridge.rs",
    "apps/aseman-node/src/drivers/vmm/proxy.rs", "apps/aseman-node/src/drivers/vmm/hostcall_entities.rs",
):
    SOURCE_REVIEWS[_source] = ("covered_by_reviewed_family", "ADR 0022 VM family key helper or delete path")
GENERIC_OWNER = "unassigned source review"


def candidate_family(template: str) -> str:
    parts = template.removeprefix("link::").removeprefix("json::").split("::")
    if parts[0] == "Json" and len(parts) > 1:
        family = f"Json::{parts[1]}"
        # `Json::CreatureNamespace::billing` style exact keys keep their third segment.
        return f"{family}::{parts[2]}" if parts[1] == "CreatureNamespace" and len(parts) > 2 else family
    return parts[0]


def classify_candidate(row: dict[str, object]) -> dict[str, object]:
    template = row["logical_template"]
    family = candidate_family(template)
    if " " in template or family == "user":
        # Human-readable messages and event author labels, not storage keys.
        return {**row, "disposition": "not_a_storage_key",
                "note": "format string is message/label text, never passed to storage"}
    if vm_disposition(family) is not None:
        return {**row, "disposition": "covered_by_reviewed_family",
                "note": "heuristic match of a reviewed family: ADR 0022 VM state"}
    if family in COVERED_CANDIDATE_FAMILIES:
        return {**row, "disposition": "covered_by_reviewed_family",
                "note": f"heuristic match of a reviewed family: {COVERED_CANDIDATE_FAMILIES[family]}"}
    if family in {"{}", ""}:
        source = row["source"].rsplit(":", 1)[0]
        disposition, note = SOURCE_REVIEWS.get(
            source,
            ("blocked_candidate_review", f"unreviewed family owned by {GENERIC_OWNER}; fails closed at export"),
        )
        return {**row, "disposition": disposition, "note": note}
    owner = next(
        (owner for prefix, owner in OWNED_BLOCKED_PREFIXES.items() if family.startswith(prefix)),
        None,
    )
    return {**row, "disposition": "blocked_candidate_review",
            "note": f"unreviewed family owned by {owner}; fails closed at export" if owner
            else "heuristic source match is not sufficient evidence of a persisted key"}


def classify_link_or_index(template: str, method: str, source: str = "") -> dict[str, object]:
    if method == "search_link_vals_list":
        # Scans `index::{type}::{column}::id::`; only Creature/username has a writer.
        if template == "Creature":
            return {"disposition": "derived_index_or_relationship", "target_kind": None,
                    "note": "reads the verified Creature username index"}
        return {"disposition": "reviewed_no_persisted_record", "target_kind": None,
                "note": "no legacy writer creates this index; any record fails closed"}
    if method in {"get_index", "put_index", "del_index", "has_index"}:
        family = template.split("::")[0] or template
        if family in UNWRITTEN_INDEX_FAMILIES:
            return {"disposition": "reviewed_no_persisted_record", "target_kind": None,
                    "note": "read/deleted only; no legacy writer creates this index"}
        if family in {name for name, _, _ in VERIFIED_INDEX_FAMILIES}:
            return {"disposition": "derived_index_or_relationship", "target_kind": None,
                    "note": "verified against reviewed objects; stale entries fail closed"}
        return {"disposition": "blocked_link_authority", "target_kind": None,
                "note": "unreviewed index family fails closed at export"}
    family = template.removeprefix("link::").split("::")[0].split("{")[0]
    if (vm := vm_disposition(family)) is not None:
        return vm
    if family == "CreatureTypeExists":
        return {"disposition": "derived_index_or_relationship", "target_kind": None,
                "note": "ADR 0016 flag verified to match the creature type registry exactly"}
    if family == "UserPrivateKey":
        return {"disposition": "intentional_removal", "target_kind": None,
                "note": "ADR 0019 custodial key verified against its creature; never exported (RL-019)"}
    if family in {"hasaccess", "onaccess"}:
        return {"disposition": "fixture_backed_transform", "target_kind": "core.store_membership",
                "note": "ADR 0018 paired membership; exact permission set; member resolved or remote"}
    if family in FINANCE_RECORD_LINKS:
        return {"disposition": "fixture_backed_transform", "target_kind": "finance.legacy_record",
                "note": "ADR 0017 counter/marker record; value and referenced record verified"}
    if family in FINANCE_DERIVED_LINKS:
        return {"disposition": "derived_index_or_relationship", "target_kind": None,
                "note": "ADR 0017 derived counter/listing; reconciled against finance records"}
    if family in VERIFIED_LINK_FAMILIES:
        return {"disposition": "derived_index_or_relationship", "target_kind": None,
                "note": "consumed or verified against reviewed objects; divergence fails closed"}
    if not family:
        return {"disposition": "covered_by_reviewed_family", "target_kind": None,
                "note": "dynamic delete path of ADR 0022 VM families; deletes cannot create records"}
    owner = next(
        (owner for prefix, owner in OWNED_BLOCKED_PREFIXES.items() if family.startswith(prefix)),
        None,
    )
    if owner:
        return {"disposition": "blocked_link_authority", "target_kind": None,
                "note": f"unreviewed family owned by {owner}; fails closed at export"}
    return {"disposition": "blocked_link_authority", "target_kind": None,
            "note": "link may hold primary state; unreviewed family fails closed at export"}


def build() -> dict[str, object]:
    source = json.loads(SOURCE.read_text())
    core_objects = []
    for row in source["core_objects"]:
        if row["object_type"] == "Program":
            disposition = "fixture_backed_transform"
            note = "strict columns; machineId resolves creature owner/relationship; unknown columns fail"
        elif row["object_type"] == "Creature":
            disposition = "fixture_backed_transform"
            note = "split into user/creature/wallet; RSA is tagged legacy multicodec; currency/scale required"
        elif row["object_type"] == "Entity":
            disposition = "fixture_backed_transform"
            note = "strict composite key and program relationship; program owner resolved server-side"
        elif row["object_type"] == "Store":
            disposition = "fixture_backed_transform"
            note = "strict binary fields; creator and optional parent links resolved from snapshot graph"
        elif row["object_type"] in {"Chain", "ChainShard"}:
            disposition = "fixture_backed_transform"
            note = "strict IDs; owner resolved through store creator; named shard identity preserved"
        elif row["object_type"] == "Session":
            disposition = "fixture_backed_transform"
            note = "legacy bearer ID becomes a domain-separated digest and immediately revoked session"
        elif row["object_type"] == "File":
            disposition = "fixture_backed_transform"
            note = "metadata emits only with bounded external-byte size and SHA-256 copy evidence"
        else:
            disposition = "requires_graph_transform"
            note = "assemble columns, resolve owner and relationships, validate required target fields"
        core_objects.append({
            **row,
            "target_kind": OBJECT_TARGETS[row["object_type"]],
            "disposition": disposition,
            "note": note,
        })
    questdb = []
    for row in source["questdb_tables"]:
        if row["name"] == "storage":
            disposition = "fixture_backed_transform"
            target_kind = "realtime.event"
            note = "rows sort into per-store sequences; scope/retention are server-resolved; tags are strict"
        elif row["name"] == "buildlogs":
            disposition = "fixture_backed_transform"
            target_kind = "telemetry.build_log"
            note = "VM owner is resolved server-side; milliseconds convert exactly to microseconds"
        else:
            disposition = "blocked_missing_target_kind"
            target_kind = None
            note = "no reviewed target kind exists"
        questdb.append({
            **row,
            "disposition": disposition,
            "target_kind": target_kind,
            "note": note,
        })
    hashgraph = [
        {
            **row,
            "disposition": "consensus_provider_checkpoint",
            "target_kind": None,
            "note": ("ADR 0025: consensus-provider private state kept by the Hashgraph module "
                     "(RL-011); strictly classified, block-digest checkpoint; decoded only by P8"),
        }
        for row in source["hashgraph_rocksdb"]
    ]
    cluster = {
        **source["cluster_rocksdb"],
        "disposition": "state_machine_checkpoint_verified",
        "target_kind": None,
        "note": ("ADR 0012: sm/state read verbatim, canonically digested, and identical across "
                 "replicas; membership is re-enrolled under ADR 0013; shared_config knobs are "
                 "surfaced for typed-configuration (A103) review; retained for rollback only"),
    }
    accesses = [classify_access(row) for row in source["application_accesses"]]
    candidates = [classify_candidate(row) for row in source["candidate_key_templates"]]
    blocked = sum(row["disposition"].startswith("blocked_") for row in accesses)
    blocked += sum(row["disposition"].startswith("blocked_") for row in candidates)
    blocked += sum(row["disposition"] != "fixture_backed_transform" for row in core_objects)
    blocked += sum(row["disposition"] != "fixture_backed_transform" for row in questdb)
    return {
        "_meta": {
            "artifact": "A308",
            "status": "IN_PROGRESS" if blocked else "ACCEPTED",
            "generator": GENERATOR,
            "last_verified_commit": retained_revision(ROOT, JSON_OUT),
            "source_digest_artifact": "docs/generated/current-storage-access.json",
        },
        "schema_version": 1,
        "identity_strategy": "sha2-256-domain-separated-first-128-bits-with-legacy-id-map",
        "unknown_record_policy": "fail_closed",
        "physical_layouts": source["physical_layouts"],
        "core_objects": core_objects,
        "questdb_tables": questdb,
        "hashgraph_families": hashgraph,
        "cluster_rocksdb": cluster,
        "application_accesses": accesses,
        "candidate_key_templates": candidates,
        "summary": {
            "application_accesses": len(accesses),
            "candidate_key_templates": len(candidates),
            "blocked_rows": blocked,
        },
    }


def markdown(data: dict[str, object]) -> str:
    counts: dict[str, int] = {}
    for row in data["application_accesses"]:
        counts[row["disposition"]] = counts.get(row["disposition"], 0) + 1
    lines = [
        "---", "status: GENERATED", "owner: migration/storage",
        f"source_of_truth: docs/generated/current-storage-access.json and {GENERATOR}",
        f"last_verified_commit: {data['_meta']['last_verified_commit'][:12]}",
        f"verification: python3 {GENERATOR} --check", "---", "",
        "# Legacy-to-capsule transform manifest", "",
        f"A004 rows are exhaustively accounted for, but A308 remains `{data['_meta']['status']}`.",
        f"There are **{data['summary']['blocked_rows']}** blocked review rows. Unknown records fail",
        "closed; a blocked or heuristic row is never copied as an opaque authoritative capsule.", "",
        "## Direct access disposition", "", "| Disposition | Rows |", "|---|---:|",
    ]
    lines.extend(f"| `{name}` | {count} |" for name, count in sorted(counts.items()))
    owners: dict[str, int] = {}
    for row in [*data["application_accesses"], *data["candidate_key_templates"]]:
        if row["disposition"].startswith("blocked_"):
            note = row["note"]
            owner = note.split("owned by ", 1)[1].split(";")[0] if "owned by " in note else "unassigned"
            owners[owner] = owners.get(owner, 0) + 1
    lines += ["", "## Blocked rows by owning phase", "", "| Owner | Rows |", "|---|---:|"]
    lines.extend(f"| {name} | {count} |" for name, count in sorted(owners.items()))
    lines += ["", "## Typed object families", "", "| Legacy family | Target | Status |", "|---|---|---|"]
    lines.extend(
        f"| `{row['object_type']}` | `{row['target_kind']}` | `{row['disposition']}` |"
        for row in data["core_objects"]
    )
    lines += [
        "", "## Reviewed JSON document families (ADR 0016)", "",
        "| Legacy key | Target |", "|---|---|",
        *(f"| `{prefix}{{id}}` at `metadata` | `{kind}` |" for prefix, kind in DOCUMENT_TARGETS.items()),
        "", "ADR 0017 exports the legacy finance subsystem as a reconciled, immutable",
        "`finance.legacy_record` epoch; derived counters and listing links are verified only.", "",
        "The next review must add fixture-backed transforms for the remaining VM/runtime links",
        "and JSON, raw operational keys, Hashgraph checkpoints, and OpenRaft state-machine",
        "authority before A308 can be accepted or any backfill can start.", "",
    ]
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    data = build()
    outputs = {
        JSON_OUT: json.dumps(data, indent=2, sort_keys=True) + "\n",
        MD_OUT: markdown(data),
    }
    stale = []
    for path, content in outputs.items():
        if args.check:
            if not path.exists() or path.read_text() != content:
                stale.append(path)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
            print(f"wrote {path.relative_to(ROOT)}")
    if stale:
        for path in stale:
            print(f"stale: {path.relative_to(ROOT)}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
