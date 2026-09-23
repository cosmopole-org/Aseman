#!/usr/bin/env python3
"""Generate the public HTTP contract (A701) from the action registry.

The public API is not written by hand. It is derived from `contracts/security/actions.json`
— the one place that already says which actions exist, who may call them, and which
surfaces they cover — so an action can never be reachable over HTTP without being
authorizable, and a surface can never drift out of the published contract.

Every legacy signed shell action becomes one `POST /v1/actions/{path}` operation. The
legacy TCP and WebSocket transports carry the same actions (A701/P7-05): they are
framing adapters over this contract, not a second API.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REGISTRY = ROOT / "contracts/security/actions.json"
ROUTES = ROOT / "docs/generated/current-routes.json"
OPENAPI_OUT = ROOT / "contracts/public/openapi.json"
MD_OUT = ROOT / "docs/generated/public-api.md"
GENERATOR = "scripts/generate_public_api.py"

SURFACE = "signed-shell-action "

# Actions whose rule is `never` are refused by policy and are not published at all: a
# published operation that can only ever be denied is a trap for a client author.
NEVER = "never"


def load() -> tuple[dict, dict]:
    return (
        json.loads(REGISTRY.read_text(encoding="utf-8")),
        json.loads(ROUTES.read_text(encoding="utf-8")),
    )


def action_by_surface(registry: dict) -> dict[str, dict]:
    """Which action covers each signed-shell-action path."""
    found: dict[str, dict] = {}
    for action in registry["actions"]:
        for surface in action.get("surfaces", []):
            if surface.startswith(SURFACE):
                found[surface[len(SURFACE) :]] = action
    return found


def operation_id(path: str) -> str:
    parts = [part for part in path.strip("/").split("/") if part]
    head, *rest = parts
    return head + "".join(part[:1].upper() + part[1:] for part in rest)


def build(registry: dict, routes: dict) -> tuple[dict, list[dict]]:
    covering = action_by_surface(registry)
    rows: list[dict] = []
    paths: dict[str, dict] = {}
    for entry in sorted(routes["signed_shell_actions"], key=lambda item: item["path"]):
        path = entry["path"]
        action = covering.get(path)
        if action is None:
            raise SystemExit(f"{path} maps to no action; the registry generator should have caught this")
        rule = action.get("rule", [])
        published = NEVER not in rule
        rows.append(
            {
                "path": path,
                "action": action["id"],
                "class": action["class"],
                "subjects": action["subjects"],
                "request_type": entry["request_type"],
                "published": published,
            }
        )
        if not published:
            continue
        paths[f"/v1/actions{path}"] = {
            "post": {
                "operationId": operation_id(path),
                "summary": action["summary"],
                "description": (
                    f"Action `{action['id']}` on `{action['resource']}`. "
                    f"Authorized subjects: {', '.join(action['subjects'])}."
                ),
                "tags": [action["resource"]],
                "security": [{"session": []}, {"proof": []}],
                "x-aseman-action": action["id"],
                "x-aseman-class": action["class"],
                "x-aseman-legacy-request": entry["request_type"],
                "requestBody": {
                    "required": True,
                    "content": {"application/json": {"schema": {"type": "object"}}},
                },
                "parameters": [
                    {
                        "name": "Aseman-Request-Id",
                        "in": "header",
                        "required": False,
                        "schema": {"type": "string"},
                        "description": "Echoed in the response and in traces.",
                    },
                    {
                        "name": "Idempotency-Key",
                        "in": "header",
                        # Everything but a read changes something: issuing a session,
                        # rotating a key, moving money. A retry of any of those must
                        # not repeat the effect.
                        "required": action["class"] != "read",
                        "schema": {"type": "string"},
                        "description": (
                            "Required for a mutating action: a retry under the same key "
                            "returns the first outcome instead of repeating the effect."
                        ),
                    },
                ],
                "responses": {
                    "200": {
                        "description": "The action's result.",
                        "content": {"application/json": {"schema": {"type": "object"}}},
                    },
                    "401": {"$ref": "#/components/responses/Problem"},
                    "403": {"$ref": "#/components/responses/Problem"},
                    "409": {"$ref": "#/components/responses/Problem"},
                    "429": {"$ref": "#/components/responses/Problem"},
                    "503": {"$ref": "#/components/responses/Problem"},
                },
            }
        }

    document = {
        "openapi": "3.1.0",
        "info": {
            "title": "Aseman public API",
            "version": "1",
            "summary": "The default public protocol (A701).",
            "description": (
                "Generated from the A402 action registry by "
                f"`{GENERATOR}`; do not edit by hand. Every operation here is one "
                "registered action, so nothing is reachable that is not authorizable. "
                "The legacy TCP and WebSocket transports carry these same actions and "
                "contain framing only."
            ),
        },
        "servers": [{"url": "https://{node}/api", "variables": {"node": {"default": "node.example"}}}],
        "components": {
            "securitySchemes": {
                "session": {
                    "type": "apiKey",
                    "in": "header",
                    "name": "Aseman-Session",
                    "description": "A session identifier. It is a bearer credential and is never mapped or logged.",
                },
                "proof": {
                    "type": "apiKey",
                    "in": "header",
                    "name": "Aseman-Proof",
                    "description": "An A401 signed request proof, for a workload or a node.",
                },
            },
            "responses": {
                "Problem": {
                    "description": "A typed problem (RFC 9457).",
                    "content": {
                        "application/problem+json": {
                            "schema": {"$ref": "#/components/schemas/Problem"}
                        }
                    },
                }
            },
            "schemas": {
                "Problem": {
                    "type": "object",
                    "required": ["type", "title", "status"],
                    "properties": {
                        "type": {"type": "string"},
                        "title": {"type": "string"},
                        "status": {"type": "integer"},
                        "detail": {"type": "string"},
                        "instance": {"type": "string"},
                        "aseman_reason": {
                            "type": "string",
                            "description": "The stable refusal reason, for a client to branch on.",
                        },
                    },
                }
            },
        },
        "paths": dict(sorted(paths.items())),
    }
    return document, rows


def markdown(document: dict, rows: list[dict]) -> str:
    published = [row for row in rows if row["published"]]
    withheld = [row for row in rows if not row["published"]]
    lines = [
        "---",
        "status: GENERATED",
        "owner: network",
        f"source_of_truth: contracts/security/actions.json via {GENERATOR}",
        f"verification: python3 {GENERATOR} --check",
        "---",
        "",
        "# Public API (A701)",
        "",
        f"> Generated by `{GENERATOR}`. Do not edit by hand.",
        "",
        "HTTP is the default public protocol. Every operation is one registered A402",
        "action, so nothing is reachable over HTTP that the policy cannot authorize.",
        "The legacy TCP and WebSocket transports carry the same actions and contain",
        "framing only.",
        "",
        f"- Published operations: {len(published)}",
        f"- Withheld (policy `never`): {len(withheld)}",
        "",
        "A mutating action requires an `Idempotency-Key`: a retry under the same key",
        "returns the first outcome instead of repeating the effect.",
        "",
        "| Operation | Action | Class | Subjects |",
        "|---|---|---|---:|",
    ]
    for row in published:
        subjects = ", ".join(row["subjects"])
        lines.append(
            f"| `POST /v1/actions{row['path']}` | `{row['action']}` | {row['class']} | {subjects} |"
        )
    if withheld:
        lines += [
            "",
            "## Withheld",
            "",
            "These surfaces exist in the legacy transports but the policy refuses them",
            "outright, so publishing them would be a trap for a client author.",
            "",
        ]
        lines += [f"- `{row['path']}` (`{row['action']}`)" for row in withheld]
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail when the outputs are stale")
    arguments = parser.parse_args()

    registry, routes = load()
    document, rows = build(registry, routes)
    openapi = json.dumps(document, indent=2) + "\n"
    reference = markdown(document, rows)

    if arguments.check:
        stale = [
            path
            for path, wanted in ((OPENAPI_OUT, openapi), (MD_OUT, reference))
            if not path.exists() or path.read_text(encoding="utf-8") != wanted
        ]
        for path in stale:
            print(f"stale: {path.relative_to(ROOT)}", file=sys.stderr)
        return 1 if stale else 0

    OPENAPI_OUT.write_text(openapi, encoding="utf-8")
    MD_OUT.write_text(reference, encoding="utf-8")
    print(f"wrote {OPENAPI_OUT.relative_to(ROOT)}")
    print(f"wrote {MD_OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
