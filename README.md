# oa

**Optimize Anything**: a single-binary CLI and MCP server that lets coding agents (Claude Code, Antigravity, Cursor, Codex, Gemini CLI) profile real bottlenecks, optimize latency or peak RAM, and accept changes only when tests, output equivalence, and interleaved A/B statistics pass.

```text
$ oa init --bench "python3 parser.py" --test "python3 -m unittest test_parser.py"
Initialized oa session in slow_log_parser
  Locked files    : 1 file(s)
  Baseline timing : 570.03 ms median (p95 670.75 ms, CV 9.1%)
  Peak RSS        : 19780 KiB
  Output SHA-256  : 2fd0cb644a42a5c7.. (286 bytes)
  Profiler        : python:cProfile+tracemalloc [CPU_ALGORITHMIC]
  Top hotspots:
    -  53.0% self ( 528.67 ms,      1 calls)  analyze_logs  (parser.py:65)

$ oa evaluate          # after editing test_parser.py
Verdict: CheatBlocked (vs best, metric Wall)
Reason : Cheat Guard blocked evaluation: protected file(s) modified or deleted (test_parser.py)

$ oa evaluate          # after replacing O(N*U) list scan with a set
Verdict: KeepRecommended (vs best, metric Wall)
Equiv  : exact output equivalence verified (exit 0, stdout sha256 2fd0cb644a42a5c7.., 0 output file(s))
Ref    : 639.82 ms median (p95 715.84 ms, MAD 18.37 ms, n=8)
Cand   : 76.59 ms median (p95 80.77 ms, MAD 2.80 ms, n=8)
Gain   : 8.09x paired (95% CI [7.89x, 9.69x], Mann-Whitney p=0.0009)
```

## Why `oa`

Coding agents are good at rewriting hot loops and bad at measuring whether the rewrite actually helped. Left alone, an agent will time one cold run against one warm run, edit a test or shrink a workload, break edge-case output, or claim a 2% speedup inside a 5% noise floor.

`oa` enforces the measurement loop outside the model:

- Cheat Guard locks tracked test, benchmark, and explicit files with SHA-256 digests at `oa init`, and stamps each `oa evaluate` result with a working-tree fingerprint so post-evaluation edits cannot be kept.
- Equivalence Gate verifies exit code plus exact SHA-256 of stdout and declared output files (with optional line-filter normalization or an external oracle command).
- Balanced interleaved A/B benchmarking builds and runs the reference version inside a detached shadow git worktree (`.oa/worktrees/best` or `baseline`), alternating `(ref, cand)` and `(cand, ref)` pairs to cancel thermal throttling and host drift. Changes are accepted only when the 2,000-resample 95% bootstrap CI lower bound exceeds the threshold and the two-sided Mann-Whitney U test passes (`p <= alpha`).
- Latency and RAM modes support `--metric wall` (median wall time in ms) and `--metric rss` (kernel peak RSS in KiB via `/usr/bin/time`, macOS `/usr/bin/time -l`, or Unix `python3` `resource.getrusage`).
- Cross-runtime profiling automatically runs Python `cProfile` + `tracemalloc` (with source snippets and allocation sites), Node.js V8 `--cpu-prof` (with JS/TS source snippets), or Linux `perf stat`/`perf record` (with automatic rootless `:u` user-space hardware counters for IPC, branch misses, and cache misses, plus `rusage` fallback).
- Non-invasive git history stores snapshots under `refs/oa/baseline`, `refs/oa/best`, and `refs/oa/exp/<id>` without polluting your branch or tracked `.gitignore` (`.oa/` is added to `.git/info/exclude`), tracks a Pareto frontier in `.oa/ledger.jsonl`, and records dead ends so agents stop retrying failed ideas.

Built in Rust with `#![forbid(unsafe_code)]`, two direct dependencies (`serde` and `serde_json`), and a 1.3 MB stripped release binary.

## Install

Linux (`x86_64`) and macOS (`arm64`) prebuilt release binary with SHA-256 verification:

```bash
curl -fsSL https://raw.githubusercontent.com/alexander-src/oa/main/install.sh | sh
```

Or build from source (Rust 1.75+):

```bash
cargo install --git https://github.com/alexander-src/oa
```

Windows (`x86_64`) prebuilt archives are attached on the [Releases](https://github.com/alexander-src/oa/releases) page.

## Wire Into Your Coding Agent

Run `oa setup` once to register the `oa mcp` stdio server and install `SKILL.md` for detected agents (Claude Code, Antigravity, Cursor, Codex, Gemini CLI). Existing configs are backed up to `*.oa.bak` first.

```bash
oa setup --dry-run   # preview changes
oa setup             # apply changes
```

## CLI Quickstart

Run `oa` directly in any git repository, or try the bundled `examples/slow_log_parser` workload:

```bash
oa init --bench "python3 parser.py" --test "python3 -m unittest test_parser.py"
oa profile
# edit code targeting the top hotspot
oa evaluate
oa checkpoint --keep --hypothesis "replace O(N*U) list scan with set"
oa profile --diff-from baseline
oa evaluate --against baseline
oa report --out optimization_report.md
```

Optimize peak memory instead of wall time:

```bash
oa init --bench "python3 pipeline.py" --metric rss
oa evaluate --metric rss
```

## Real Verified Runs

Both runs below were executed on Linux `x86_64` (AMD Ryzen 7 9800X3D, 16 CPUs) using the compiled `oa 0.1.0` binary.

### 1. `brave/adblock-rust` (`c4fb340e`, production Rust adblock engine)

Workload: compile 134,208 real EasyList + EasyPrivacy rules (`easylist.txt` + `easyprivacy.txt`) and match 30,000 real HTTP requests from `data/requests.json`, gated by all 246 upstream library tests (`cargo test --lib`) and an exact SHA-256 digest over every block/allow/exception/redirect/rewrite decision (`54cd0d0e524b4f9d`). `oa init` locked 102 test/bench files and profiled with Linux `perf` (3.05 IPC, 0.6% baseline CV).

| # | Status | Hypothesis | Paired Gain | 95% CI | p-value | Median ms |
| ---: | --- | --- | ---: | --- | ---: | ---: |
| 1 | KEPT | Replace one-shot `memmem::FinderBuilder` with anchor-byte `memchr` scan for short URL/hostname needles | 1.03x | [1.02x, 1.03x] | 0.0001 | 136.97 |
| 2 | REVERTED | Monomorphic ASCII tokenizer (`NoiseReject`: LLVM already devirtualized) | 1.00x | [1.00x, 1.01x] | 0.2855 | 137.21 |
| 3 | REVERTED | Sort all tokens (`RegressionReject`: placed wildcard bucket `0` first) | 0.77x | [0.75x, 0.78x] | 0.0000 | 182.51 |
| 4 | REVERTED | Sort non-zero tokens (`RegressionReject`: broke URL-order hostname early exit) | 0.79x | [0.78x, 0.80x] | 0.0000 | 173.85 |
| 5 | KEPT | Lazy `FlatNetworkFilter::patterns()`, `FxHashMap` domain table, skip empty maps/tags, remove duplicate URL clone | 1.02x | [1.02x, 1.03x] | 0.0000 | 134.58 |
| 6 | REVERTED | L1-sized `ShortHash` Bloom filter (`NoiseReject`: init cost offset 30k-request savings) | 1.01x | [1.00x, 1.03x] | 0.1988 | 135.81 |

Direct 20-pair interleaved head-to-head (`oa evaluate --against baseline --pairs 20`): **142.73 ms -> 136.09 ms** (**1.05x**, 95% CI `[1.03x, 1.07x]`, Mann-Whitney `p = 0.0002`, 246/246 upstream tests passing, identical SHA-256 `54cd0d0e524b4f9d`).

### 2. Bundled `examples/slow_log_parser` (Python)

Reproducible directly from this repository (`python3 parser.py`, `python3 -m unittest test_parser.py`, stdout SHA-256 `2fd0cb644a42a5c7`):

- Experiment #1 (edit `test_parser.py`): blocked immediately by Cheat Guard (`CheatBlocked`) and reverted.
- Experiment #2 (replace O(N*U) list membership scan with `set`): kept at **8.09x** (`p = 0.0009`).
- Experiment #3 (regex micro-optimization while `generate_logs` dominated remaining time): rejected as `NoiseReject` (`1.15x`, 95% CI `[0.95x, 1.22x]`, `p = 0.0409`) and reverted.
- Final 10-pair head-to-head against baseline: **512.68 ms -> 55.70 ms** (**9.13x**, 95% CI `[8.63x, 9.45x]`, Mann-Whitney `p = 0.0002`).

## MCP Tools

`oa mcp` runs a stdio JSON-RPC 2.0 server supporting both newline-delimited JSON and `Content-Length` framing.

| Tool | Purpose |
| --- | --- |
| `oa_init` | Lock protected files, snapshot baseline/best worktrees, record output SHA-256, and return initial profile |
| `oa_profile` | Profile current tree (`auto`, `python`, `node`, `perf`, `native`) and optionally diff hotspots vs `baseline` or `best` |
| `oa_evaluate` | Run Cheat Guard, build, tests, equivalence gate, and balanced interleaved A/B evaluation (`wall` or `rss`) |
| `oa_checkpoint` | Keep verified working tree into `refs/oa/best`, or revert and log a dead end in `.oa/ledger.jsonl` |
| `oa_ledger` | Return experiment history, cumulative gain, Pareto frontier IDs, and ruled-out dead ends |
| `oa_report` | Render a PR-ready Markdown or JSON optimization report |

## Limitations

- `oa` requires a git repository with at least one commit so shadow worktrees and `refs/oa/*` snapshots can isolate baseline vs candidate code.
- `--metric rss` requires Linux or macOS (`/usr/bin/time` or `python3` `resource.getrusage`); Windows currently supports `--metric wall` only.
- Native line-level symbol profiling currently uses Linux `perf` (with rootless `:u` fallback). macOS (`sample`/`xctrace`), Windows (ETW), Go (`pprof`), and JVM/Ruby adapters are not implemented yet.
- Sub-10 ms workloads are dominated by process startup jitter; wrap microbenchmarks in an inner loop so each invocation runs at least 50 to 200 ms.
- Exact equivalence mode expects deterministic stdout and output files; use `--ignore-line <substr>` or `--oracle <cmd>` for workloads that print timestamps or nondeterministic ordering.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the crate layout and the 5-step guide to adding a new language/OS profiler adapter or coding agent setup target.

## License

MIT. See [LICENSE](LICENSE).
