#!/usr/bin/env python3
"""Dependency-free bounded load probe for the A701 HTTP edge."""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import statistics
import time
import urllib.error
import urllib.request
import uuid
from collections import Counter


def percentile(values: list[float], quantile: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int((len(ordered) - 1) * quantile))]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", required=True)
    parser.add_argument("--path", default="/v1/actions/api/ping")
    parser.add_argument("--session-env", default="ASEMAN_LOAD_SESSION")
    parser.add_argument("--requests", type=int, default=1000)
    parser.add_argument("--concurrency", type=int, default=32)
    parser.add_argument("--p95-ms", type=float, default=1000)
    parser.add_argument("--max-error-rate", type=float, default=0.001)
    parser.add_argument("--timeout-seconds", type=float, default=30)
    parser.add_argument("--mutation", action="store_true")
    parser.add_argument("--body", default="{}")
    args = parser.parse_args()
    if args.requests < 1 or args.concurrency < 1:
        parser.error("requests and concurrency must be positive")
    session = os.environ.get(args.session_env)
    if not session:
        parser.error(f"{args.session_env} is required")
    try:
        body = json.dumps(json.loads(args.body), separators=(",", ":")).encode()
    except json.JSONDecodeError as error:
        parser.error(f"--body is not JSON: {error}")
    endpoint = args.url.rstrip("/") + args.path

    def request(index: int) -> tuple[int, float, bool]:
        headers = {
            "Accept": "application/json",
            "Content-Type": "application/json",
            "Aseman-Session": session,
            "Aseman-Request-Id": f"load-{index}-{uuid.uuid4()}",
        }
        if args.mutation:
            headers["Idempotency-Key"] = f"load-{index:016d}-{uuid.uuid4()}"
        started = time.perf_counter()
        status = 0
        equal_replay = True
        try:
            req = urllib.request.Request(endpoint, data=body, headers=headers, method="POST")
            with urllib.request.urlopen(req, timeout=args.timeout_seconds) as response:
                status = response.status
                first = response.read()
            if args.mutation:
                with urllib.request.urlopen(req, timeout=args.timeout_seconds) as response:
                    equal_replay = response.status == status and response.read() == first
        except urllib.error.HTTPError as error:
            status = error.code
            error.read()
        except (OSError, TimeoutError):
            status = 0
        return status, (time.perf_counter() - started) * 1000, equal_replay

    started = time.time()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrency) as pool:
        results = list(pool.map(request, range(args.requests)))
    durations = [result[1] for result in results]
    statuses = Counter(result[0] for result in results)
    errors = sum(count for status, count in statuses.items() if status < 200 or status >= 300)
    replay_mismatches = sum(not result[2] for result in results)
    error_rate = errors / len(results)
    p95 = percentile(durations, 0.95)
    passed = error_rate <= args.max_error_rate and p95 <= args.p95_ms and replay_mismatches == 0
    report = {
        "scenario": "public-http-deployment-load",
        "started_at": int(started),
        "duration_seconds": round(time.time() - started, 3),
        "environment": {"url": args.url, "path": args.path, "mutation": args.mutation},
        "measurements": {
            "requests": len(results),
            "status_counts": dict(sorted(statuses.items())),
            "error_rate": error_rate,
            "p50_millis": percentile(durations, 0.50),
            "p95_millis": p95,
            "p99_millis": percentile(durations, 0.99),
            "replay_mismatches": replay_mismatches,
        },
        "thresholds": {"maximum_error_rate": args.max_error_rate, "p95_millis": args.p95_ms, "replay_mismatches": 0},
        "passed": passed,
    }
    print(json.dumps(report, sort_keys=True))
    raise SystemExit(0 if passed else 1)


if __name__ == "__main__":
    main()
