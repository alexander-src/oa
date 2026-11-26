---
name: oa
description: Profile-guided, equivalence-gated, statistically verified performance and memory optimization loop using the oa MCP server or CLI.
---

# Optimize Anything (`oa`) Playbook

Use `oa` whenever a user asks to optimize latency, throughput, CPU usage, or memory footprint (Peak RSS) in any repository. Never guess at bottlenecks, never trust single un-interleaved timings, and never edit locked tests or benchmark harnesses.

## MCP Tools (10 total)

| Tool | Purpose |
| --- | --- |
| `oa_init` | Lock test/bench files, snapshot `refs/oa/baseline` and `refs/oa/best`, capture output SHA-256, run initial profile |
| `oa_profile` | Profile active session or standalone `cmd` (`auto`, `python`, `node`, `go`, `perf`, `sample`, `native`) with source snippets, allocations, and `--diff-from` deltas |
| `oa_evaluate` | Run Cheat Guard -> build -> locked tests -> SHA-256 equivalence -> balanced interleaved A/B pairs (`wall` or `rss`) |
| `oa_checkpoint` | Keep a verified win (`refs/oa/best` advances) or revert to best and log a dead end in `.oa/ledger.jsonl` |
| `oa_verify` | Fresh end-to-end verification of `HEAD` vs `refs/oa/baseline` (15 interleaved pairs, Cheat Guard, tests, SHA-256 badge) |
| `oa_status` | Live session metric, commits, cumulative gain, lock health, dirty files, and `git diff --stat` vs baseline |
| `oa_ledger` | Full experiment history, Pareto frontier IDs, and ruled-out dead ends |
| `oa_report` | Generate a PR-ready Markdown report, raw JSON ledger, or dark/light SVG chart (`format: "svg"`) |
| `oa_diff` | Export the clean unified patch and diff stat between `refs/oa/baseline` and `refs/oa/best` |
| `oa_compare` | Standalone interleaved A/B benchmark of two commands (`cmd_a`/`cmd_b`) or git revisions (`git_a`/`git_b` + `bench_cmd`) without initializing a session |

## Standard Optimization Loop

1. Initialize once (`oa_init`). Pass a deterministic `bench_cmd` (prefer workloads >= 30 ms so process startup noise does not dilute gains) and `test_cmd` (upstream unit tests). For compiled projects (Rust, C, C++, Go, TypeScript bundles), pass `build_cmd` with debug symbols (`-g` or `CARGO_PROFILE_RELEASE_DEBUG=true`) so both candidate and shadow worktrees rebuild cleanly and `linux:perf` resolves ELF `file:line` source snippets. For memory optimization on Linux, macOS, or Windows, set `metric: "rss"`. Never edit any file listed in `locked_files`.
2. Read the bottleneck class before editing. Check `profile.bottleneck_class`, `profile.hotspots`, `profile.alloc_sites`, and `profile.counters` (IPC, branch-miss %, cache-miss %). Pick one focused hypothesis targeting the top hotspot by self time or allocation volume.
3. Apply the matching tactic for the bottleneck class:
   - `CPU_ALGORITHMIC` / `COMPUTE_BOUND`: eliminate O(N^2) scans (sets, dicts, index maps), replace per-token `strncmp`/regex cascades with `switch` or lookup tables, cache loop-invariant lookups, fast-path common ASCII/integer cases, use SIMD or stack batching on hot byte loops, and split cold metadata out of hot structs (SoA).
   - `SERIALIZATION_BOUND`: precompile combined regexes, hoist character-class checks with 256-byte LUTs, avoid intermediate AST/event object allocations by using flat typed arrays or single-pass streaming, advance output buffer offsets directly instead of calling `strlen`.
   - `ALLOC_PRESSURE` / `GC_OR_ALLOC_BOUND` / `--metric rss`: eliminate per-item heap allocations (stack buffers, packed typed arrays, lazy optional structures), prefer `Copy` borrows (`Option<&T>`) over enums that carry `Drop` glue on hot paths, stream input/output instead of materializing full lists, and pre-size output buffers.
   - `LOCK_CONTENTION`: shrink critical sections, batch updates outside locks, use sharded or thread-local accumulators.
   - `IO_OR_SYSCALL`: batch reads/writes, buffer I/O, hoist repeated `stat`/`open`/locale calls outside hot loops.
4. Evaluate every change (`oa_evaluate`) and act on the verdict without rationalizing around it:
   - `KEEP_RECOMMENDED`: call `oa_checkpoint(action: "keep", hypothesis: "...")`, then re-profile with `oa_profile(diff_from: "best")` to target the next shifted hotspot.
   - `NOISE_REJECT` or `REGRESSION_REJECT`: call `oa_checkpoint(action: "revert", hypothesis: "...", dead_end: "...")` immediately. Never retry a logged dead end.
   - `TESTS_FAIL` or `EQUIVALENCE_FAIL`: fix the semantic bug (ANSI codes, whitespace, floating-point edge cases, locale rules) or revert. Never weaken tests or switch equivalence policy to hide a regression.
   - `CHEAT_BLOCKED`: revert any accidental edit to locked test/bench files before continuing.
5. Verify, export, and clean up. Stop once two consecutive hypotheses on the top remaining hotspot produce `NOISE_REJECT` or remaining time is dominated by startup or saturated hardware. Run `oa_verify` against `baseline`, export the clean patch (`oa_diff`) and report (`oa_report` in Markdown and optional SVG), and run `oa clean --refs` once the patch is committed.

## Hard Guardrails

- Keep diffs small, idiomatic, and reviewable for upstream PRs; do not add new runtime dependencies.
- Never claim a speedup without citing the exact `oa_verify` or `oa_evaluate` median, 95% CI, Mann-Whitney `p`, and equivalence SHA-256.
- Report ruled-out dead ends honestly in the final summary.
