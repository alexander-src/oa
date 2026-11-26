# Showcase: `examples/ram_aggregator` (8.34x less RAM with `--metric rss`)

`oa` is not limited to wall-clock latency. Passing `--metric rss` switches the optimization objective, interleaved A/B statistics, and verdict gate to **Peak Resident Set Size (KiB)**, so coding agents can hunt down memory bloat with the same Cheat Guard and SHA-256 equivalence guarantees.

The unoptimized baseline is bundled directly in this repository at [examples/ram_aggregator/](../../examples/ram_aggregator/) so you can reproduce the entire session locally in under 15 seconds.

## What `oa init --metric rss` diagnoses

Running `oa init --metric rss` on [aggregate.py](../../examples/ram_aggregator/aggregate.py) flags the allocation bottleneck automatically:

```text
  Baseline timing : 270.81 ms median (p95 272.07 ms, CV 0.7%)
  Peak RSS        : 163920 KiB
  Output SHA-256  : 6f5b1b2e2a7795e6.. (1013 bytes)
  Profiler        : python:cProfile+tracemalloc [ALLOC_PRESSURE]
  Diagnosis       : Peak traced allocations reached 135.9 MiB while generate_lines accounts for 31.1% self time; reduce temporary copies or stream iterators.
```

## Head-to-head results (10 balanced interleaved pairs)

Replacing the materialized 120,000-line list, full decoded dict list, and per-service copies with a streaming generator and `O(services)` in-place accumulators cuts Peak RSS by **88.0%** while keeping every JSON decode and output byte identical (`sha256: 6f5b1b2e2a7795e6`):

| Metric | Baseline | After `oa` | Delta |
| --- | ---: | ---: | ---: |
| Peak RSS (`--metric rss`) | 163,870 KiB (160.0 MiB) | 19,668 KiB (19.2 MiB) | **8.34x less RAM (-88.0%)**, 95% CI `[8.31x, 8.36x]`, `p = 0.0002` |
| Median wall time (`--metric wall`) | 272.27 ms | 212.15 ms | **1.29x faster**, 95% CI `[1.27x, 1.32x]`, `p = 0.0002` |
| Locked tests | 2/2 pass | 2/2 pass | `test_aggregate.py` locked via SHA-256 |

## Reproduce locally

```bash
cp -r examples/ram_aggregator /tmp/ram-demo && cd /tmp/ram-demo
git init && git add . && git commit -m "baseline"
oa init --metric rss --bench "python3 aggregate.py" --test "python3 test_aggregate.py" --pairs 10
```

Then ask your coding agent to profile and minimize Peak RSS using `oa_profile`, `oa_evaluate`, and `oa_checkpoint`.
