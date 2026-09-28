#!/usr/bin/env python3
"""Generate the typed storage client (ADR 0036) from the model schemas.

Reads `contracts/capsule/kinds/*-logical-schemas.json` and writes
`crates/aseman-storage/src/client.rs`: one module per model with its record type,
its `Create` input, typed field handles for filters and ordering, an update builder,
and unique selectors, plus the `Models` extension that gives a transaction one
accessor per model (`trx.store()`, `trx.finance_wallet()`).

Usage: generate_storage_client.py [--check]
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
KINDS = ROOT / "contracts/capsule/kinds"
OUT = ROOT / "crates/aseman-storage/src/client.rs"
SOURCES = ["core-logical-schemas.json", "storage-class-logical-schemas.json"]

RUST_TYPES = {
    "text": "String",
    "integer": "i64",
    "timestamp_micros": "i64",
    "float": "f64",
    "bool": "bool",
    "bytes": "Vec<u8>",
    "capsule_id": "Id",
    "document": "serde_json::Value",
}
KEYWORDS = {
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false",
    "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
    "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true",
    "type", "unsafe", "use", "where", "while", "async", "await", "dyn", "abstract",
    "become", "box", "do", "final", "macro", "override", "priv", "typeof",
    "unsized", "virtual", "yield", "try", "gen",
}
RESERVED = {"id", "revision", "record_created_micros", "record_updated_micros"}


def ident(name: str) -> str:
    return f"r#{name}" if name in KEYWORDS else name


def pascal(name: str) -> str:
    return "".join(part.capitalize() for part in name.split("_"))


def load() -> list[dict]:
    # A kind declared in both files takes its storage-class definition, as the
    # runtime catalog does (the later source wins).
    models: dict[str, dict] = {}
    for source in SOURCES:
        data = json.loads((KINDS / source).read_text(encoding="utf-8"))
        for model in data["definitions"]:
            models[model["kind"]] = model
    return [models[kind] for kind in sorted(models)]


def accessor(kind: str) -> str:
    namespace, name = kind.split(".", 1)
    return name if namespace == "core" else f"{namespace}_{name}"


def members(model: dict) -> list[tuple[str, str, bool]]:
    """(name, rust type, required) for every field and relation."""
    required = set(model.get("required", []))
    out = []
    for name, kind in model["fields"].items():
        if name in RESERVED:
            raise ValueError(f"{model['kind']}: {name} is a reserved record member")
        out.append((name, RUST_TYPES[kind], name in required))
    for name in model.get("relationships", {}):
        out.append((name, "Id", False))
    return out


def model_module(model: dict) -> str:
    kind = model["kind"]
    record = pascal(kind.split(".", 1)[1])
    fields = members(model)
    lines = [
        f"    /// `{kind}`.",
        f"    pub mod {ident(kind.split('.', 1)[1])} {{",
        "        #![allow(clippy::all, unused_imports)]",
        "        use crate::error::StorageResult;",
        "        use crate::query::Unique;",
        "        use crate::typed::{self, Field, Update};",
        "        use crate::value::{Data, Id, Row, Value};",
        "",
        f'        pub const NAME: &str = "{kind}";',
        "",
        f"        /// A `{kind}` record.",
        "        #[derive(Clone, Debug, PartialEq)]",
        f"        pub struct {record} {{",
        "            pub id: Id,",
        "            pub revision: u64,",
        "            /// When the record was created and last written (not model fields).",
        "            pub record_created_micros: i64,",
        "            pub record_updated_micros: i64,",
    ]
    for name, rust, required in fields:
        lines.append(f"            pub {ident(name)}: {rust if required else f'Option<{rust}>'},")
    lines += [
        "        }",
        "",
        "        /// The values `create` takes.",
        "        #[derive(Clone, Debug, Default, PartialEq)]",
        "        pub struct Create {",
    ]
    for name, rust, required in fields:
        lines.append(f"            pub {ident(name)}: {rust if required else f'Option<{rust}>'},")
    lines += [
        "        }",
        "",
        "        impl From<Create> for Data {",
        "            fn from(create: Create) -> Data {",
        "                let mut data = Data::new();",
    ]
    for name, _, required in fields:
        if required:
            lines.append(
                f'                data.insert("{name}".to_owned(), Value::from(create.{ident(name)}));'
            )
        else:
            lines.append(f"                if let Some(value) = create.{ident(name)} {{")
            lines.append(f'                    data.insert("{name}".to_owned(), Value::from(value));')
            lines.append("                }")
    lines += [
        "                data",
        "            }",
        "        }",
        "",
        f"        impl typed::Model for {record} {{",
        "            const NAME: &'static str = NAME;",
        "            type Create = Create;",
        "",
        "            fn from_row(row: Row) -> StorageResult<Self> {",
        "                Ok(Self {",
    ]
    for name, _, required in fields:
        if required:
            lines.append(f'                    {ident(name)}: typed::required(&row, NAME, "{name}")?,')
        else:
            lines.append(f'                    {ident(name)}: typed::optional(&row, "{name}"),')
    lines += [
        "                    id: row.id,",
        "                    revision: row.revision,",
        "                    record_created_micros: row.created_at_micros,",
        "                    record_updated_micros: row.updated_at_micros,",
        "                })",
        "            }",
        "        }",
        "",
        "        /// The record id.",
        "        #[must_use]",
        '        pub const fn id() -> Field<Id> {',
        '            Field::new("id")',
        "        }",
    ]
    for name, rust, _ in fields:
        lines += [
            "",
            "        #[must_use]",
            f"        pub const fn {ident(name)}() -> Field<{rust}> {{",
            f'            Field::new("{name}")',
            "        }",
        ]
    lines += [
        "",
        "        /// An update: set fields, or clear optional ones with `None`.",
        "        #[derive(Clone, Debug, Default, PartialEq)]",
        "        pub struct Updater(Update);",
        "",
        "        #[must_use]",
        "        pub fn update() -> Updater {",
        "            Updater::default()",
        "        }",
        "",
        "        impl Updater {",
    ]
    for name, rust, required in fields:
        if required:
            lines += [
                "            #[must_use]",
                f"            pub fn {ident(name)}(self, value: impl Into<{rust}>) -> Self {{",
                f'                Self(self.0.set("{name}", value.into()))',
                "            }",
            ]
        else:
            lines += [
                "            #[must_use]",
                f"            pub fn {ident(name)}(self, value: Option<{rust}>) -> Self {{",
                "                match value {",
                f'                    Some(value) => Self(self.0.set("{name}", value)),',
                f'                    None => Self(self.0.clear("{name}")),',
                "                }",
                "            }",
            ]
    lines += [
        "        }",
        "",
        "        impl From<Updater> for Update {",
        "            fn from(updater: Updater) -> Update {",
        "                updater.0",
        "            }",
        "        }",
        "",
        "        #[must_use]",
        "        pub fn by_id(id: Id) -> Unique {",
        "            Unique::Id(id)",
        "        }",
    ]
    if model.get("key_family"):
        lines += [
            "",
            "        /// The record with this natural key.",
            "        #[must_use]",
            "        pub fn by_key(key: impl Into<String>) -> Unique {",
            "            Unique::Key(key.into())",
            "        }",
        ]
    types = dict((name, rust) for name, rust, _ in fields)
    for index in model.get("unique_indexes", []):
        if index == ["key"] and model.get("key_family"):
            continue
        name = "by_" + "_and_".join(index)
        params = ", ".join(f"{ident(field)}: impl Into<{types[field]}>" for field in index)
        pairs = ", ".join(f'("{field}", Value::from({ident(field)}.into()))' for field in index)
        lines += [
            "",
            "        #[must_use]",
            f"        pub fn {name}({params}) -> Unique {{",
            f"            Unique::fields([{pairs}])",
            "        }",
        ]
    lines.append("    }")
    return "\n".join(lines)


def render(models: list[dict]) -> str:
    namespaces: dict[str, list[dict]] = {}
    for model in models:
        namespaces.setdefault(model["kind"].split(".", 1)[0], []).append(model)
    out = [
        "// Generated by scripts/generate_storage_client.py from the model schemas in",
        "// contracts/capsule/kinds; do not edit by hand.",
        "//! The typed storage client (ADR 0036): one module per model, and [`Models`],",
        "//! which gives a transaction one accessor per model.",
        "#![allow(clippy::too_many_lines, clippy::module_name_repetitions)]",
        "",
        "use crate::engine::Trx;",
        "use crate::typed::ModelClient;",
        "",
    ]
    for namespace, members_ in sorted(namespaces.items()):
        out.append(f"pub mod {ident(namespace)} {{")
        out.append("\n\n".join(model_module(model) for model in members_))
        out.append("}")
        out.append("")
    out += [
        "/// One accessor per model on a transaction.",
        "pub trait Models {",
    ]
    for model in models:
        namespace, name = model["kind"].split(".", 1)
        record = pascal(name)
        out.append(f"    fn {accessor(model['kind'])}(&self) -> ModelClient<'_, {ident(namespace)}::{ident(name)}::{record}>;")
    out += ["}", "", "impl Models for Trx {"]
    for model in models:
        namespace, name = model["kind"].split(".", 1)
        record = pascal(name)
        out += [
            f"    fn {accessor(model['kind'])}(&self) -> ModelClient<'_, {ident(namespace)}::{ident(name)}::{record}> {{",
            "        ModelClient::new(self)",
            "    }",
        ]
    out += ["}", ""]
    return "\n".join(out)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    arguments = parser.parse_args()
    text = render(load())
    if arguments.check:
        if not OUT.exists() or OUT.read_text(encoding="utf-8") != text:
            print("stale: crates/aseman-storage/src/client.rs", file=sys.stderr)
            return 1
        print("storage client is up to date")
        return 0
    OUT.write_text(text, encoding="utf-8")
    print(f"wrote {OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
