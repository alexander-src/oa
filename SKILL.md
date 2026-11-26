---
name: oa
description: Profile, optimize, and verify code performance with the oa MCP server using Cheat Guard, output equivalence checks, and interleaved A/B benchmarking.
---

# oa Optimization Workflow

Use the `oa` MCP tools whenever asked to speed up code, reduce memory usage, or profile a slow command or test workload.

## Loop

1. Start a session with `oa_init`, passing `bench_cmd` (plus `test_cmd` and `build_cmd` when present). Read the returned `profile.hotspots`, `profile.alloc_sites`, and `profile.diagnosis` before editing code.
2. Check `oa_ledger` if resuming an existing session so you never repeat a recorded dead end.
3. Form one concrete hypothesis targeting the top hotspot (for example: replace an O(N^2) list membership scan with a set, hoist regex compilation out of a loop, remove redundant allocations, or reduce syscalls). Make one clean, focused edit per experiment.
4. Run `oa_evaluate` to verify Cheat Guard locks, build, test suite, output SHA-256 equivalence, and interleaved A/B timing statistics.
5. Decide based strictly on `oa_evaluate`:
   - If `verdict` is `KEEP_RECOMMENDED`, call `oa_checkpoint` with `action: "keep"` and a concise `hypothesis`.
   - If `verdict` is `NOISE_REJECT`, `REGRESSION_REJECT`, `EQUIVALENCE_FAIL`, `TESTS_FAIL`, or `BUILD_FAIL`, call `oa_checkpoint` with `action: "revert"`, your `hypothesis`, and a short `dead_end` note explaining what happened.
6. After keeping a change, call `oa_profile` with `diff_from: "baseline"` or `diff_from: "best"` to see which hotspot moved to the top next, then repeat from step 3 until the target goal is reached or diminishing returns set in.
7. Before finishing, run `oa_evaluate` with `compare_against: "baseline"` for a direct end-to-end A/B check, then call `oa_report` to produce the PR-ready summary.

## Rules

- Never edit files locked by Cheat Guard (tests, benchmarks, or fixtures). `oa_evaluate` and `oa_checkpoint` reject tampered files automatically.
- Never claim a speedup from a single run or from profiler wall time. Only cite numbers returned by `oa_evaluate` with `KEEP_RECOMMENDED` (95% CI lower bound above threshold and Mann-Whitney `p <= alpha`).
- Keep each experiment atomic so the ledger attributes every speedup or dead end to a single idea.
