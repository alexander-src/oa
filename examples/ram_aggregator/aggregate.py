"""Bundled `--metric rss` demo for oa: bloated in-memory event aggregator.

Baseline behavior (intentionally memory-heavy, common in Python ETL scripts):
1. Materializes all 120,000 raw event lines into a Python list.
2. Parses every line into a full dict and keeps all 120,000 dicts alive in RAM.
3. Copies all per-service payloads into grouped lists before reducing them.

Run `oa init --metric rss --bench "python3 aggregate.py" --test "python3 test_aggregate.py"`
and let an agent stream the records to slash Peak RSS with exact SHA-256 equivalence.
"""

from __future__ import annotations

import hashlib
import json

SERVICES = ("auth", "billing", "catalog", "checkout", "edge", "search", "storage", "worker")
STATUSES = (200, 200, 200, 201, 204, 400, 404, 429, 500, 503)


def generate_lines(count: int = 120_000) -> list[str]:
    lines = []
    for i in range(count):
        svc = SERVICES[(i * 7 + 3) % len(SERVICES)]
        status = STATUSES[(i * 13 + 5) % len(STATUSES)]
        latency_us = 120 + ((i * 2654435761) & 0xFFFF) % 45_000
        payload_bytes = 256 + ((i * 37) % 4096)
        region = "eu-west" if (i & 1) == 0 else "us-east"
        lines.append(
            f'{{"id":{i},"service":"{svc}","region":"{region}",'
            f'"status":{status},"latency_us":{latency_us},"bytes":{payload_bytes}}}'
        )
    return lines


def summarize_events(count: int = 120_000) -> dict[str, dict[str, int]]:
    # Anti-pattern 1: materialize every raw JSON line in memory up front.
    raw_lines = generate_lines(count)

    # Anti-pattern 2: decode and retain all 120k dict records simultaneously.
    records = [json.loads(line) for line in raw_lines]

    # Anti-pattern 3: group full record copies by service before aggregating.
    grouped: dict[str, list[dict[str, object]]] = {}
    for rec in records:
        grouped.setdefault(str(rec["service"]), []).append(dict(rec))

    summary: dict[str, dict[str, int]] = {}
    for svc in sorted(grouped):
        bucket = grouped[svc]
        latencies = [int(r["latency_us"]) for r in bucket]
        total_bytes = sum(int(r["bytes"]) for r in bucket)
        errors = sum(1 for r in bucket if int(r["status"]) >= 500)
        summary[svc] = {
            "count": len(bucket),
            "errors": errors,
            "total_bytes": total_bytes,
            "min_us": min(latencies),
            "max_us": max(latencies),
            "sum_us": sum(latencies),
        }
    return summary


def main() -> None:
    summary = summarize_events(120_000)
    rendered = json.dumps(summary, sort_keys=True)
    digest = hashlib.sha256(rendered.encode("utf-8")).hexdigest()[:16]
    print(f"services={len(summary)} sha256={digest} {rendered}")


if __name__ == "__main__":
    main()
