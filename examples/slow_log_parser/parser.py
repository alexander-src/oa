#!/usr/bin/env python3
"""Deterministic synthetic HTTP access-log analyzer with realistic bottlenecks."""

import argparse
import json
import re
import sys

ENDPOINTS = [
    "/api/v1/users",
    "/api/v1/orders",
    "/api/v1/search",
    "/api/v1/checkout",
    "/api/v1/sessions",
    "/api/v1/metrics",
    "/api/v1/export",
    "/api/v1/webhooks",
]
STATUSES = [200, 200, 200, 200, 201, 204, 304, 400, 401, 404, 500, 502, 503]


def generate_logs(count: int) -> list[str]:
    """Generate deterministic log lines without external fixture files."""
    state = 0x12345678
    lines = []
    for i in range(count):
        state = (1103515245 * state + 12345) & 0x7FFFFFFF
        ip_a = 10
        ip_b = (state >> 16) % 16
        ip_c = (state >> 8) % 64
        ip_d = (state % 250) + 1
        ep = ENDPOINTS[(state >> 5) % len(ENDPOINTS)]
        status = STATUSES[(state >> 11) % len(STATUSES)]
        latency_ms = ((state >> 3) % 450) + 1
        req_id = f"req-{i % 1800:04d}"
        lines.append(
            f"2026-11-26T09:{(i // 3600) % 24:02d}:{(i // 60) % 60:02d}Z "
            f"ip={ip_a}.{ip_b}.{ip_c}.{ip_d} method=GET path={ep} "
            f"status={status} latency_ms={latency_ms} id={req_id}"
        )
    return lines


def parse_line(line: str) -> dict | None:
    """Parse a single log line (intentionally recompiles regexes per line)."""
    pattern = (
        r"^(?P<ts>\S+)\s+ip=(?P<ip>\d+\.\d+\.\d+\.\d+)\s+"
        r"method=(?P<method>[A-Z]+)\s+path=(?P<path>\S+)\s+"
        r"status=(?P<status>\d+)\s+latency_ms=(?P<latency>\d+)\s+id=(?P<id>\S+)$"
    )
    m = re.match(pattern, line)
    if not m:
        return None
    return {
        "ts": m.group("ts"),
        "ip": m.group("ip"),
        "method": m.group("method"),
        "path": m.group("path"),
        "status": int(m.group("status")),
        "latency": int(m.group("latency")),
        "id": m.group("id"),
    }


def analyze_logs(lines: list[str]) -> dict:
    """Aggregate status buckets, unique IPs, and 5xx error counts."""
    status_buckets: dict[str, int] = {}
    unique_ips: list[str] = []
    error_by_path: dict[str, int] = {}
    total_latency = 0
    parsed = 0

    for line in lines:
        rec = parse_line(line)
        if rec is None:
            continue
        parsed += 1
        total_latency += rec["latency"]

        bucket = f"{rec['status'] // 100}xx"
        status_buckets[bucket] = status_buckets.get(bucket, 0) + 1

        # Intentional O(N * U) list membership scan for benchmarking/profiling demo.
        if rec["ip"] not in unique_ips:
            unique_ips.append(rec["ip"])

        if rec["status"] >= 500:
            p = rec["path"]
            error_by_path[p] = error_by_path.get(p, 0) + 1

    top_errors = sorted(error_by_path.items(), key=lambda kv: (-kv[1], kv[0]))[:5]
    return {
        "parsed": parsed,
        "unique_ips": len(unique_ips),
        "total_latency_ms": total_latency,
        "status_buckets": dict(sorted(status_buckets.items())),
        "top_5xx_paths": top_errors,
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Analyze synthetic HTTP access logs.")
    ap.add_argument("--lines", type=int, default=12000)
    args = ap.parse_args(argv)
    lines = generate_logs(args.lines)
    summary = analyze_logs(lines)
    print(json.dumps(summary, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
