#!/usr/bin/env python3
"""Check the legacy TCP and WebSocket transports carry framing only (A701, P7-05).

The Phase 7 gate requires that the legacy transports "contain framing only and share
one session/application path". That is a property of the code, so it is checked here
rather than asserted in a document: the adapters may frame bytes, manage their own
sockets, and hand the body to the one session path — and nothing else.

A transport that grew its own action lookup, its own authorization, or its own storage
access would be a second application path with its own semantics, which is exactly the
thing this phase removes.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SESSION = ROOT / "apps/aseman-node/src/drivers/network/client/session.rs"
ADAPTERS = [
    ROOT / "apps/aseman-node/src/drivers/network/client/tcp.rs",
    ROOT / "apps/aseman-node/src/drivers/network/client/ws.rs",
]

# What an adapter must not do for itself. Each one, if it appeared, would mean the
# transport had opinions about the application rather than about bytes.
FORBIDDEN = {
    "action lookup": re.compile(r"\bactions?\(\)|find_action|ISecureAction\b"),
    "authorization": re.compile(r"\bauthorize|policy\(\)|decide\(|guard\("),
    "storage access": re.compile(r"\btrx\(\)|put_link|get_link|put_json|del_key"),
    "session classification": re.compile(r"classify_session_route"),
    "rate-limit policy": re.compile(r"RateLimitKey::|rate_limited_body"),
}

# The one path an adapter hands an inbound body to.
DELEGATES = re.compile(r"session::process_inbound")


def check() -> list[str]:
    problems: list[str] = []
    if not SESSION.exists():
        return [f"{SESSION.relative_to(ROOT)} is gone: the transports have no shared path"]
    session = SESSION.read_text(encoding="utf-8")
    if not DELEGATES.search("session::process_inbound") or "fn process_inbound" not in session:
        problems.append("the session path no longer exposes process_inbound")

    for adapter in ADAPTERS:
        if not adapter.exists():
            problems.append(f"{adapter.relative_to(ROOT)} is gone; update this check")
            continue
        name = adapter.relative_to(ROOT)
        source = adapter.read_text(encoding="utf-8")
        # Ignore comments: a line explaining why something is not done here is fine.
        code = "\n".join(
            line for line in source.splitlines() if not line.lstrip().startswith("//")
        )
        if not DELEGATES.search(code):
            problems.append(f"{name} does not hand inbound bodies to the shared session path")
        for what, pattern in FORBIDDEN.items():
            found = pattern.search(code)
            if found:
                line = code[: found.start()].count("\n") + 1
                problems.append(f"{name}:{line} does its own {what}; that belongs to the session path")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="accepted for symmetry")
    parser.parse_args()
    problems = check()
    for problem in problems:
        print(f"legacy transports: {problem}", file=sys.stderr)
    if problems:
        return 1
    print("legacy transports carry framing only")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
