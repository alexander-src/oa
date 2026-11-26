# Showcase: `brave/adblock-rust` (Linux `perf`, 1.05x gain, 4 bad edits blocked)

[brave/adblock-rust](https://github.com/brave/adblock-rust) is Brave Browser's production Rust adblock engine. Unlike unoptimized Python code, it is already heavily tuned native Rust running at **3.05 instructions per cycle** on real EasyList + EasyPrivacy filter sets.

This case study shows how `oa` behaves when a codebase is already tight: rootless Linux `perf` hardware counters (`cycles:u`, `instructions:u`, `branch-misses:u`) identify realistic micro-optimizations, while interleaved A/B benchmarking catches regressions and statistical noise that would otherwise slip into a PR.

- Upstream commit: [`c4fb340e`](https://github.com/brave/adblock-rust/commit/c4fb340e)
- Verified patch: [adblock-rust.patch](adblock-rust.patch)
- Workload: 134,208 real EasyList + EasyPrivacy rules matched against 30,000 real HTTP requests
- Upstream test suite: `cargo test --lib` (246/246 passed)
- Locked files: 102 files (tests, benches, and Cargo manifests)
- Output SHA-256: `54cd0d0e524b4f9d` (identical match count and blocker decisions across all 30,000 requests)

## Head-to-head result (20 balanced interleaved pairs)

| Metric | Baseline | After `oa` | Delta |
| --- | ---: | ---: | ---: |
| Median wall time | 142.73 ms | 136.09 ms | **1.05x** (95% CI `[1.03x, 1.07x]`, `p = 0.0002`) |
| Upstream unit tests | 246/246 pass | 246/246 pass | 0 regressions |
| Output SHA-256 | `54cd0d0e524b4f9d` | `54cd0d0e524b4f9d` | Exact byte match |

## Why the rejected experiments matter

Out of 7 candidate patches generated during the session, `oa evaluate` rejected 4 that sounded reasonable from reading the source code:

| Outcome | Count | What `oa` measured |
| --- | ---: | --- |
| `KeepRecommended` | 3 | Verified wins combining to `1.05x` (`p = 0.0002`) across 20 interleaved pairs |
| `RegressionReject` | 2 | Caught `0.77x` and `0.79x` slowdowns (`p = 0.0000`) from extra cache misses and indirection |
| `NoiseReject` | 2 | Rejected `p = 0.2855` and `p = 0.1988` edits whose deltas fell inside run-to-run variance |

Without interleaved A/B worktrees and Mann-Whitney U gating, an agent running single-shot `time` commands easily keeps noisy or regressive micro-optimizations on already-fast Rust code.
