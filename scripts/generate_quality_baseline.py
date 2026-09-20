#!/usr/bin/env python3
"""Generate reproducible static quality metrics for migration baseline A009."""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter
from pathlib import Path

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
JSON_PATH = ROOT / "docs/generated/current-quality-baseline.json"
MARKDOWN_PATH = ROOT / "docs/generated/current-quality-baseline.md"
IGNORED = {".git", "target", "node_modules", "dist", "__pycache__"}
SOURCE_SUFFIXES = {".rs", ".ts", ".js", ".py", ".sh"}
PATTERNS = {
    "rust_allow_attributes": re.compile(r"#!?\s*\[allow\("),
    "rust_unsafe_tokens": re.compile(r"\bunsafe\b"),
    "rust_unwrap_calls": re.compile(r"\.unwrap\s*\("),
    "rust_expect_calls": re.compile(r"\.expect\s*\("),
    "rust_panic_macros": re.compile(r"\b(?:panic|todo|unimplemented)!\s*\("),
    "rust_sleep_calls": re.compile(r"(?:thread::sleep|tokio::time::sleep)\s*\("),
    "rust_spawn_calls": re.compile(r"(?:thread::spawn|tokio::spawn)\s*\("),
    "rust_json_value_mentions": re.compile(r"serde_json::Value|\bValue\b"),
    "environment_reads": re.compile(r"(?:std::)?env::var\s*\("),
}


def relative(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def source_files() -> list[Path]:
    return sorted(
        path
        for path in ROOT.rglob("*")
        if path.is_file()
        and path.suffix in SOURCE_SUFFIXES
        and not any(part in IGNORED for part in path.relative_to(ROOT).parts)
    )


def build() -> dict[str, object]:
    files = source_files()
    suffixes: Counter[str] = Counter()
    totals: Counter[str] = Counter()
    largest: list[dict[str, object]] = []
    pattern_locations: dict[str, list[str]] = {name: [] for name in PATTERNS}

    for path in files:
        text = path.read_text(encoding="utf-8", errors="replace")
        lines = text.splitlines()
        code_lines = sum(bool(line.strip()) for line in lines)
        suffixes[path.suffix] += 1
        totals["physical_lines"] += len(lines)
        totals["nonblank_lines"] += code_lines
        largest.append({"path": relative(path), "lines": len(lines)})

        if path.suffix == ".rs":
            for name, pattern in PATTERNS.items():
                for line_number, line in enumerate(lines, 1):
                    matches = list(pattern.finditer(line))
                    if not matches:
                        continue
                    totals[name] += len(matches)
                    if len(pattern_locations[name]) < 25:
                        pattern_locations[name].append(
                            f"{relative(path)}:{line_number}"
                        )

    return {
        "_meta": {
            "artifact": "A009",
            "status": "CURRENT",
            "source_of_truth": "repository source files outside generated/build directories",
            "last_verified_commit": retained_revision(ROOT, JSON_PATH),
            "generator": relative(Path(__file__)),
        },
        "source": {
            "file_count": len(files),
            "files_by_suffix": dict(sorted(suffixes.items())),
            "physical_lines": totals["physical_lines"],
            "nonblank_lines": totals["nonblank_lines"],
            "largest_files": sorted(
                largest, key=lambda row: (-int(row["lines"]), str(row["path"]))
            )[:20],
        },
        "ratchet_counts": {
            name: {"count": totals[name], "sample_locations": locations}
            for name, locations in pattern_locations.items()
        },
        "limitations": [
            "Lexical counts include tests/comments and are ratchets, not defect counts.",
            "Runtime performance and recovery measurements are recorded in docs/migration/baseline.md.",
            "Duplicate-code and cyclomatic-complexity tooling is introduced by Phase 1 xtask/CI.",
        ],
    }


def markdown(data: dict[str, object]) -> str:
    meta = data["_meta"]
    source = data["source"]
    ratchets = data["ratchet_counts"]
    lines = [
        "---",
        "status: CURRENT",
        "owner: migration/P0-04",
        f"source_of_truth: {meta['generator']}",
        f"last_verified_commit: {meta['last_verified_commit'][:12]}",
        f"verification: python3 {meta['generator']} --check",
        "---",
        "",
        "# Current static quality baseline",
        "",
        f"Scanned {source['file_count']} source files, {source['physical_lines']} physical lines, and {source['nonblank_lines']} nonblank lines.",
        "",
        "## Ratchet counts",
        "",
        "| Metric | Count |",
        "|---|---:|",
    ]
    for name, detail in sorted(ratchets.items()):
        lines.append(f"| `{name}` | {detail['count']} |")
    lines += [
        "",
        "These lexical metrics include tests and comments. They establish a reproducible",
        "ratchet; they do not assert that every occurrence is defective.",
        "",
        "## Largest source files",
        "",
        "| Path | Lines |",
        "|---|---:|",
    ]
    for row in source["largest_files"]:
        lines.append(f"| `{row['path']}` | {row['lines']} |")
    lines += ["", "## Limitations", ""]
    lines.extend(f"- {item}" for item in data["limitations"])
    lines.append("")
    return "\n".join(lines)


def write_or_check(path: Path, value: str, check: bool) -> bool:
    if check:
        return path.exists() and path.read_text(encoding="utf-8") == value
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(value, encoding="utf-8")
    return True


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    data = build()
    json_value = json.dumps(data, indent=2, sort_keys=True) + "\n"
    markdown_value = markdown(data)
    ok_json = write_or_check(JSON_PATH, json_value, args.check)
    ok_markdown = write_or_check(MARKDOWN_PATH, markdown_value, args.check)
    if args.check and not (ok_json and ok_markdown):
        print("quality baseline is stale; regenerate without --check", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
