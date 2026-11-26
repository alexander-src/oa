---
name: oa
description: Profile-guided, equivalence-gated, statistically verified performance and memory optimization loop using the oa MCP server or CLI.
---

# Optimize Anything (`oa`) Playbook

Use `oa` whenever a user asks to optimize latency, throughput, CPU usage, or memory footprint (Peak RSS) in any repository. Never guess at bottlenecks, never trust single un-interleaved timings, and never edit locked tests or benchmark harnesses.

## MCP Tools (9 total)

| Tool | Purpose |
| --- | --- |
| `oa_init` | Lock test/bench files, snapshot `refs/oa/baseline` and `refs/oa/best`, capture output SHA-256, run initial profile |
| `oa_profile` | Profile current code (`auto`, `python`, `node`, `go`, `perf`, `sample`, `native`) with snippets, allocations, and `--diff-from` deltas |
| `oa_evaluate` | Run Cheat Guard -> build -> locked tests -> SHA-256 equivalence -> balanced interleaved A/B pairs (`wall` or `rss`) |
| `oa_checkpoint` | Keep a verified win (`refs/oa/best` advances) or revert to best and log a dead end in `.oa/ledger.jsonl` |
| `oa_verify` | Fresh end-to-end verification of `HEAD` vs `refs/oa/baseline` (15 interleaved pairs, Cheat Guard, tests, SHA-256 badge) |
| `oa_status` | Live session metric, commits, cumulative gain, lock health, dirty files, and `git diff --stat` vs baseline |
| `oa_ledger` | Full experiment history, Pareto frontier IDs, and ruled-out dead ends |
| `oa_report` | Generate a PR-ready Markdown report, raw JSON ledger, or dark/light SVG chart (`format: "svg"`) |
| `oa_compare` | Standalone interleaved A/B benchmark of any two commands without initializing a session |

## Standard Optimization Loop

1. **Initialize once (`oa_init`):**
   - Pass a deterministic `bench_cmd` (prefer workloads >= 30 ms so process startup noise does not dilute gains) and `test_cmd` (upstream unit tests).
   - For compiled projects (Rust, C, C++, Go, TypeScript bundles), always pass `build_cmd` so both the candidate and shadow reference worktrees rebuild cleanly.
   - For memory optimization, set `metric: "rss"`.
   - Never modify any file listed in `locked_files`.

2. **Read the bottleneck class before editing:**
   - Check `profile.bottleneck_class`, `profile.hotspots`, `profile.alloc_sites`, and `profile.counters` (IPC, branch-miss %, cache-miss %).
   - Pick ONE focused hypothesis targeting the #1 hotspot by self time or allocation volume.

3. **Apply the right tactic for the bottleneck class:**
   - `CPU_ALGORITHMIC` / `COMPUTE_BOUND`: eliminate O(N^2) scans (replace list membership/removal with sets, dicts, or index maps), replace per-token `strncmp`/regex cascades with `switch` or lookup tables, cache loop-invariant lookups, fast-path common ASCII/integer cases before general fallbacks.
   - `SERIALIZATION_BOUND`: precompile combined regexes, hoist character-class checks with `Uint8Array`/256-byte LUTs, avoid intermediate AST/event object allocation by using flat typed arrays or single-pass streaming, advance output buffer offsets directly instead of calling `strlen` after writes.
   - `ALLOC_PRESSURE` / `GC_OR_ALLOC_BOUND` / `--metric rss`: eliminate per-item heap allocations (use stack buffers for small temporaries, packed typed arrays instead of object arrays, lazy allocation for rare optional metadata), stream input/output instead of materializing full lists in RAM, reuse scratch buffers across iterations.
   - `LOCK_CONTENTION`: shrink critical sections, batch updates outside locks, use sharded or thread-local accumulators.
   - `IO_OR_SYSCALL`: batch reads/writes, buffer I/O, hoist repeated `stat`/`open`/locale calls outside hot loops.

4. **Evaluate every change (`oa_evaluate`):**
   - Act on the returned verdict without rationalizing around it:
     - `KEEP_RECOMMENDED`: call `oa_checkpoint(action: "keep", hypothesis: "...")`, then re-profile with `oa_profile(diff_from: "best")` to target the next shifted hotspot.
     - `NOISE_REJECT` or `REGRESSION_REJECT`: call `oa_checkpoint(action: "revert", hypothesis: "...", dead_end: "...")` immediately. Never re-try a logged dead end.
     - `TESTS_FAIL` or `EQUIVALENCE_FAIL`: fix the semantic bug (ANSI codes, whitespace, floating-point edge cases, locale rules) or revert. Never weaken tests or switch equivalence policy to hide a regression.
     - `CHEAT_BLOCKED`: revert any accidental edit to locked test/bench files before continuing.

5. **Know when to stop and verify (`oa_verify` + `oa_report`):**
   - Stop once two consecutive hypotheses on the top remaining hotspot produce `NOISE_REJECT` or once remaining time is dominated by runtime startup or already-saturated hardware (high IPC, low miss rates).
   - Run `oa_verify` (default 15 interleaved pairs against `baseline`) to produce an unbiased end-to-end verification stamp.
   - Export the Markdown summary (`oa_report(format: "markdown")`) and optional SVG chart (`oa_report(format: "svg", theme: "dark")`).

## Hard Guardrails

- Keep diffs small, idiomatic, and reviewable for upstream PRs; do not add new runtime dependencies.
- Never claim a speedup without citing the exact `oa_verify` or `oa_evaluate` median, 95% CI, Mann-Whitney `p`, and equivalence SHA-256.
- Report ruled-out dead ends honestly in the final summary.
