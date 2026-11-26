---
name: oa
description: Verified code optimization loop using the oa MCP server (profiling, Cheat Guard test locking, SHA-256 equivalence gating, interleaved A/B benchmarking for wall latency or peak RSS). Use whenever the user asks to optimize, speed up, profile, reduce memory/RAM, or benchmark code.
---

# oa: Optimize Anything Workflow

Use the `oa` MCP tools whenever the user asks to optimize latency, throughput, or memory usage (`--metric rss`) in a repository. Never rely on ad-hoc `time` runs or unverified micro-benchmarks.

## Available MCP tools

| Tool | When to call |
| --- | --- |
| `oa_init` | Start a session: snapshot `refs/oa/baseline`, lock tests/benchmarks with SHA-256, capture reference output hash and initial profile. Pass `metric: "rss"` for memory optimization. |
| `oa_profile` | Profile the working tree (`python`, `node`, `go`, `perf`, `sample`, or `auto`), classify the bottleneck, read source snippets, and diff hotspots with `diff_from: "baseline"` or `"best"`. |
| `oa_evaluate` | Verify Cheat Guard locks, run `build_cmd` and `test_cmd`, enforce SHA-256 output equivalence, and run balanced interleaved A/B pairs against `best` or `baseline`. |
| `oa_checkpoint` | `action: "keep"` advances `refs/oa/best`; `action: "revert"` restores `refs/oa/best` and logs a `dead_end` so later iterations do not repeat failed ideas. |
| `oa_status` | Inspect active session metric, cumulative gain, lock health, dirty files, and `git diff --stat` vs baseline. |
| `oa_ledger` | Review kept experiments, Pareto frontier IDs, and ruled-out dead ends before planning the next hypothesis. |
| `oa_report` | Generate a PR-ready Markdown or JSON optimization report with host specs, CIs, p-values, SHA-256 equivalence, and dead ends. |
| `oa_compare` | Run a standalone interleaved A/B comparison of any two commands (`cmd_a`, `cmd_b`, optional `check_output: true`) without initializing a session. |

## Core loop

1. Call `oa_init` once with a deterministic `bench_cmd` (printing a hash or deterministic output summary) and `test_cmd` (the real test suite). Lock any extra benchmark scripts via `lock_files`.
2. Read the initial `oa_profile` output and focus strictly on the top 1 to 3 hotspots by self/cumulative time or traced allocations. Never optimize a function below 5% share while larger bottlenecks remain.
3. Check `oa_ledger` dead ends before proposing a new edit so you never retry a rejected approach.
4. Make one focused hypothesis per iteration in source files only. Never edit locked tests, benchmarks, or fixtures (Cheat Guard returns `CheatBlocked` and cannot be bypassed).
5. Call `oa_evaluate`.
   - If `verdict == "KeepRecommended"`, call `oa_checkpoint(action="keep", hypothesis="...")`, then re-run `oa_profile(diff_from="baseline")` to expose the new top bottleneck.
   - If `verdict` is `TestsFail`, `EquivalenceFail`, or `BuildFail`, inspect the reported error and either fix the bug cleanly or revert.
   - If `verdict` is `NoiseReject` or `RegressionReject`, call `oa_checkpoint(action="revert", hypothesis="...", dead_end="...")` with the exact ratio and p-value.
6. Stop when the top remaining hotspots are fixed interpreter/runtime startup or two consecutive well-targeted hypotheses fail to clear significance.
7. Run one final head-to-head check `oa_evaluate(compare_against="baseline", pairs=20)` and finish with `oa_report`.

## Bottleneck playbooks

Look at `bottleneck_class` and the hotspot call counts from `oa_profile`:

| Signal | Where big wins (2x to 10x+) usually hide |
| --- | --- |
| `CPU_ALGORITHMIC` | Quadratic loops (`in list` inside a loop, repeated `''.join(node.itertext())`, repeated `list.pop(0)`), redundant passes over the same data (parsing or measuring the same input 2 to 3 times before rendering), rebuilding invariant objects/regexes inside inner loops. |
| Regex / parser hotspots | Missing literal pre-checks before regexes (`if ":" not in text`, `if "[" not in data`), scanning every character in Python instead of finding delimiter positions in C, combining 20+ sequential regexes or adding a fast path for common ASCII tokens, caching case-insensitive keyword lookups. |
| `ALLOC_PRESSURE` / `GC_OR_ALLOC_BOUND` (`metric: "rss"`) | Materializing full datasets into lists/dicts before reducing (replace with streaming generators and in-place accumulators), copying containers on every recursive call, deep-copying AST/DOM nodes when read-only inspection suffices, unslotted hot objects. |
| `SERIALIZATION_BOUND` | Repeated string formatting, recursive generator `flatten()` calls where an iterative list join works, per-node `tag.lower()` / `getattr` dispatch where a precomputed class dict or `frozenset` works. |
| `LOCK_CONTENTION` | Acquiring mutexes per item instead of batching work outside the lock. |
| `IO_OR_SYSCALL` | Per-line flushes or tiny unbuffered reads/writes; batch into larger buffers. |

## Verification discipline

- Preserve public API signatures, generator/iterator types if tests assert them, and exact output bytes (including whitespace and ANSI reset boundaries).
- Keep caches scoped cleanly (per-render or invalidated on mutation) so repeated calls with different inputs or options never leak stale state.
- Report exact measured numbers from `oa_evaluate` and `oa_report` (baseline vs best median, 95% CI, Mann-Whitney p-value, peak RSS delta, test counts, and SHA-256). Never round up or omit rejected experiments.
