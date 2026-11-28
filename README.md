<div align="center">

# oa

**Optimize Anything: profiling, equivalence gating, and interleaved A/B benchmarking for coding agents.**

[Quick start](#quick-start) · [Showcases](#showcases) · [How oa works](#how-oa-works) · [GitHub Action](#github-action) · [MCP & CLI](#mcp--cli) · [FAQ](#faq)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/oa-hero-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="docs/assets/oa-hero-light.svg">
  <img alt="Verified multi-language speedups and peak RSS reductions measured with oa" src="docs/assets/oa-hero-dark.svg" width="880">
</picture>

</div>

Coding agents are good at rewriting code and bad at proving it got faster. Left to run ad-hoc `time` commands, they guess at bottlenecks, keep changes inside run-to-run noise, drop edge-case handling, or quietly loosen assertions to make a benchmark pass.

`oa` is a single Rust binary (`1.5 MB`, `#![forbid(unsafe_code)]`, 2 direct dependencies) that runs as a local CLI, a stdio MCP server, and a reusable GitHub Action. It locks your tests and benchmark scripts with SHA-256 hashes, profiles Python, Node.js, Go, Linux `perf` (with ELF `file:line` source snippets), and macOS `sample`, verifies exact output equivalence, runs balanced interleaved A/B pairs against a detached git shadow worktree of your baseline, tracks wall time and peak RSS on Linux, macOS, and Windows, and exports PR-ready patches, Markdown tables, and SVG charts.

## Quick start

### Install

```bash
curl -fsSL https://raw.githubusercontent.com/alexander-src/oa/main/install.sh | sh
```

Or build from source with Cargo:

```bash
cargo install --git https://github.com/alexander-src/oa.git
```

Prebuilt binaries for Linux, Windows, and macOS (with `SHA256SUMS.txt`) are attached to [GitHub Releases](https://github.com/alexander-src/oa/releases).

### Try it in 1 second

Run the self-contained demo (creates a temp git repo, exercises Cheat Guard and Equivalence Gate, verifies a 6x+ speedup, and cleans up):

```bash
oa demo
```

Check local profiler backends, RSS support, and installed coding agents:

```bash
oa doctor
```

### Connect your agent (MCP)

Register `oa` as an MCP server and install its optimization playbook (`SKILL.md`) in one command (auto-detects Claude Code, Antigravity, Cursor, Gemini CLI, and Codex):

```bash
oa setup
```

> **Try asking your agent:** *"Use oa to initialize an optimization session in this repo (`bench_cmd`: `python3 bench.py`, `test_cmd`: `pytest -q`). Profile the hot path, iterate on the top bottlenecks, revert anything that fails Cheat Guard or Mann-Whitney U, run `oa_verify`, export `oa_diff`, and generate a final `oa_report`."*

### Use from the terminal (CLI)

```bash
# Lock tests/bench, snapshot refs/oa/baseline, capture reference SHA-256 and initial profile
oa init --bench "python3 bench.py" --test "pytest -q"

# Inspect hotspots with source snippets (or diff against baseline after edits)
oa profile --top 12

# Run Cheat Guard -> tests -> SHA-256 equivalence -> balanced interleaved A/B
oa evaluate --pairs 12

# Keep verified win (updates refs/oa/best) or revert and log the dead end
oa checkpoint --keep --hypothesis "pack parser events in Int32Array tape"
oa checkpoint --revert --hypothesis "switch lexer to finditer" --dead-end "0.96x regression"

# End-to-end verification gate (exits non-zero on tamper, test failure, or noise)
oa verify --pairs 20

# Export a PR-ready Markdown table, SVG chart, and clean git patch
oa report --out REPORT.md
oa report --format svg --theme dark --out bench.svg
oa diff --out optimization.patch

# Remove .oa/ worktrees and refs once your patch is committed (preserves working tree edits)
oa clean --refs
```

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/oa-terminal-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="docs/assets/oa-terminal-light.svg">
    <img alt="Real oa demo and oa verify terminal output" src="docs/assets/oa-terminal-dark.svg" width="820">
  </picture>
</p>

Need a quick interleaved A/B comparison or hotspot profile without initializing a session? Run `oa compare` on any two commands or git revisions (pass `--markdown` for a paste-ready PR table, `--svg` for a chart, `--csv` for raw interleaved pair samples, or `--env KEY=VAL` for cross-platform environment variables), or run `oa profile --cmd` directly:

```bash
# Compare your working tree (or a branch) against main in detached worktrees
oa compare --git main --bench "cargo run --release" --build "cargo build --release" --pairs 15 --check-output --markdown --svg compare.svg --csv pairs.csv

# Compare any two shell commands
oa compare "python3 old.py" "python3 new.py" --env PYTHONOPTIMIZE=1 --pairs 15 --check-output --markdown

# One-shot profile with source snippets (no oa init required)
oa profile --cmd "python3 bench.py" --top 10
```

To reduce memory footprint instead of latency, pass `--metric rss` to `oa init`, `oa evaluate`, `oa verify`, or `oa compare` (supported on Linux, macOS, and Windows). Bundled walkthroughs live under [`examples/slow_log_parser`](examples/slow_log_parser) (`--metric wall`) and [`examples/ram_aggregator`](examples/ram_aggregator) (`--metric rss`).

## Showcases

Full session ledgers, profile diffs, hardware counters, rejected dead ends, and apply-ready patches across Python, Rust, C (`-O3`), and TypeScript/Node.js:

<table>
  <thead>
    <tr>
      <th>Target</th>
      <th>Verified result</th>
      <th>Verification</th>
      <th>Patch</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td><a href="docs/showcases/rich.md"><strong>Textualize/rich</strong></a><br><sub>Python · 57k+ stars · <code>cProfile</code></sub></td>
      <td><strong>3.69x</strong> Table render (<code>1047.8&nbsp;→&nbsp;283.6&nbsp;ms</code>)<br><strong>2.49x</strong> CLI · <strong>-20.3% peak RSS</strong> · <code>p=0.0000</code></td>
      <td>954/954 pytest pass<br>exact ANSI SHA-256</td>
      <td><a href="docs/showcases/rich.patch"><code>rich.patch</code></a></td>
    </tr>
    <tr>
      <td><a href="docs/showcases/tera.md"><strong>Keats/tera</strong></a><br><sub>Rust · 4.4k+ stars · <code>linux:perf</code> srcline</sub></td>
      <td><strong>2.98x</strong> suite (<code>130.5&nbsp;→&nbsp;43.9&nbsp;ms</code>, <code>5.27x</code> table)<br><strong>3.54x</strong> escape · <strong>-72.4% insns</strong> · <code>p=0.0000</code></td>
      <td>99/99 cargo tests pass<br>caught 1 regression (<code>0.92x</code>)</td>
      <td><a href="docs/showcases/tera.patch"><code>tera.patch</code></a></td>
    </tr>
    <tr>
      <td><a href="docs/showcases/cjson.md"><strong>DaveGamble/cJSON</strong></a><br><sub>C (<code>-O3</code>) · 12k+ stars · <code>linux:perf</code></sub></td>
      <td><strong>1.89x</strong> parse+print (<code>104.2&nbsp;→&nbsp;55.6&nbsp;ms</code>)<br><strong>-39.7% insns</strong> · <strong>-55.7% br-miss</strong> · <code>5.0 IPC</code></td>
      <td>19/19 CTest pass (<code>-Werror</code>)<br>180 locked files · 1 file changed</td>
      <td><a href="docs/showcases/cjson.patch"><code>cjson.patch</code></a></td>
    </tr>
    <tr>
      <td><a href="docs/showcases/js-yaml.md"><strong>nodeca/js-yaml</strong></a><br><sub>TS/Node · 80M+ npm/wk · <code>v8-cpu-prof</code></sub></td>
      <td><strong>1.77x</strong> <code>yaml.load</code> 6.9 MB (<code>78.3&nbsp;→&nbsp;44.2&nbsp;ms</code>)<br><strong>1.37x</strong> CLI · <strong>-32.8% peak RSS</strong> (<code>-58.5 MiB</code>)</td>
      <td>1,633 core + spec tests pass<br>caught 1 noise edit (<code>p=0.0404</code>)</td>
      <td><a href="docs/showcases/js-yaml.patch"><code>js-yaml.patch</code></a></td>
    </tr>
    <tr>
      <td><a href="docs/showcases/python-markdown.md"><strong>Python-Markdown</strong></a><br><sub>Python · MkDocs engine · <code>cProfile</code></sub></td>
      <td><strong>1.92x</strong> pure <code>convert</code> (<code>497.3&nbsp;→&nbsp;258.8&nbsp;ms</code>)<br><strong>1.53x</strong> CLI (<code>138.4&nbsp;→&nbsp;90.3&nbsp;ms</code>) · O(N^2) TOC fix</td>
      <td>1,230/1,230 unittest pass<br>caught 1 noise edit (<code>p=0.1572</code>)</td>
      <td><a href="docs/showcases/python-markdown.patch"><code>markdown.patch</code></a></td>
    </tr>
    <tr>
      <td><a href="docs/showcases/adblock-rust.md"><strong>brave/adblock-rust</strong></a><br><sub>Rust · 134k rules · <code>linux:perf</code></sub></td>
      <td><strong>1.05x</strong> (<code>142.7&nbsp;→&nbsp;136.1&nbsp;ms</code>, <code>p=0.0002</code>)<br><code>3.05 IPC</code> on already-tuned Rust</td>
      <td>246/246 cargo tests pass<br>blocked 2 regressions + 2 noise</td>
      <td><a href="docs/showcases/adblock-rust.patch"><code>adblock.patch</code></a></td>
    </tr>
    <tr>
      <td><a href="docs/showcases/ram-aggregator.md"><strong>examples/ram_aggregator</strong></a><br><sub>Python · bundled <code>--metric rss</code> demo</sub></td>
      <td><strong>8.34x less Peak RSS</strong> (<code>160.0&nbsp;→&nbsp;19.2&nbsp;MiB</code>)<br><code>-88.0%</code> RAM · <code>1.29x</code> wall speedup</td>
      <td>exact SHA-256 (<code>6f5b1b2e</code>)<br><code>p=0.0002</code></td>
      <td><a href="examples/ram_aggregator"><code>examples/</code></a></td>
    </tr>
  </tbody>
</table>

Running `oa report --format svg --theme dark` on the `Keats/tera` session produces this chart automatically:

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/oa-report-example-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="docs/assets/oa-report-example-light.svg">
    <img alt="Auto-generated oa report SVG chart from the Keats/tera session" src="docs/assets/oa-report-example-dark.svg" width="820">
  </picture>
</p>

## How oa works

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/oa-loop-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="docs/assets/oa-loop-light.svg">
    <img alt="The five-stage oa optimization loop" src="docs/assets/oa-loop-dark.svg" width="840">
  </picture>
</p>

`oa` enforces five controls in order on every optimization step:

1. Profile first (`oa profile`). Picks the right backend for your command and returns ranked hotspots with source snippets, allocation sites, hardware counters, bottleneck class (`CPU_ALGORITHMIC`, `SERIALIZATION_BOUND`, `ALLOC_PRESSURE`, `GC_OR_ALLOC_BOUND`, `LOCK_CONTENTION`, `IO_OR_SYSCALL`), and `--diff-from` hotspot deltas.
2. Cheat Guard (`oa init` + `oa evaluate`). Hashes tracked test and benchmark files into `.oa/locks.json`. Editing any locked file triggers `CheatBlocked`, and `oa checkpoint --keep` rejects any post-evaluation working tree change via a full tree SHA-256 fingerprint.
3. Behavioral Equivalence Gate. Checks exit code and SHA-256 of `stdout`, `stderr`, and declared `--output-file` artifacts before timing (with `--ignore-line` for dynamic timestamps or `--oracle` for custom validators).
4. Balanced interleaved A/B. Alternates `A-B` and `B-A` pairs between your working tree and a detached git shadow worktree under `.oa/worktrees/` so CPU frequency scaling and background load cancel out. Cumulative session gains chain paired ratios rather than dividing un-interleaved medians taken minutes apart.
5. Statistical verdict and verification gate. Accepts a change (`KeepRecommended`) only when two-sided Mann-Whitney U `p <= alpha` (default `0.01`) and the 2,000-resample bootstrap 95% CI lower bound exceeds `--min-speedup` (default `1.03x`). `oa verify` re-checks `HEAD` against `refs/oa/baseline` from scratch and exits non-zero on failure.

### How oa compares

| Capability | `oa` | `hyperfine` | `criterion` / `pytest-benchmark` | Agent ad-hoc `time` |
| --- | :---: | :---: | :---: | :---: |
| Balanced interleaved A/B pairs vs git baseline | Yes | No (sequential) | Separate runs | No |
| Automatic test and benchmark Cheat Guard locks | Yes | No | No | No |
| Byte-level output SHA-256 equivalence gate | Yes | No | No | No |
| Built-in cross-language profiler + source snippets | Yes | No | No | No |
| Peak RSS (`--metric rss`) on Linux, macOS, Windows | Yes | No | Partial | No |
| Stdio MCP server + `SKILL.md` for coding agents | Yes (10 tools) | No | No | No |
| PR-ready Markdown, dark/light SVG, and git patch export | Yes | Markdown only | HTML / JSON | No |

### Supported profiler backends

| Language / Runtime | Backend | Extracted signals |
| --- | --- | --- |
| Python | `python:cProfile+tracemalloc` | Function self/cum ms, call counts, source snippets, `tracemalloc` top allocation sites, peak RSS |
| Node.js / TS | `node:v8-cpu+heap-prof` | V8 `.cpuprofile` sample tree, V8 `--heap-prof` allocation sites, GC overhead, source file/line hotspots |
| Go (`go test`) | `go:pprof+mem` | `go test -cpuprofile` + `-memprofile` (`pprof -alloc_space`), GC/lock class, source snippets |
| Linux native (Rust, C, C++) | `linux:perf` | `perf stat` (IPC, branch-miss %, cache-miss %), `perf report` ELF `file:line` hotspots, `perf page-faults` RSS-growth sites, `pahole -P` packable DWARF structs, rootless `:u` fallback |
| macOS native (Rust, C, C++) | `macos:sample` | `/usr/bin/sample` top-of-stack symbol frames with automatic `macos:rusage` fallback |

## GitHub Action

Drop `uses: alexander-src/oa@v0.7.0` into any workflow to gate pull requests against their base branch on statistically verified speedups, peak RSS, and output SHA-256 equivalence (posts results to `$GITHUB_STEP_SUMMARY` and fails on regression or output mismatch):

```yaml
- uses: alexander-src/oa@v0.7.0
  with:
    mode: git-compare
    base-ref: ${{ github.event.pull_request.base.sha }}
    build-cmd: cargo build --release
    bench-cmd: ./target/release/my_bench
    pairs: 15
    check-output: true
    fail-on-regression: true
```

Also supports `mode: compare` (any two commands via `cmd-a` and `cmd-b`) and `mode: verify` (verifies an active `.oa/` session against `refs/oa/baseline`). See [`action.yml`](action.yml) for all inputs and outputs (`verdict`, `ratio`, `p-value`).

## MCP & CLI

`oa mcp` speaks JSON-RPC 2.0 over stdio (both NDJSON and `Content-Length` framing) and exposes 11 tools:

| MCP Tool | CLI Equivalent | Purpose |
| --- | --- | --- |
| `oa_discover` | `oa discover` | Zero-config repo scanner inferring languages, build/test commands, and benchmark candidates |
| `oa_init` | `oa init` | Lock test/bench files, snapshot `refs/oa/baseline`, capture output SHA-256 and baseline profile |
| `oa_profile` | `oa profile` | Profile current tree (or standalone `--cmd`), classify bottleneck, list heap sites and `pahole` struct padding, diff vs `baseline` or `best` |
| `oa_evaluate` | `oa evaluate` (`bench`) | Enforce Cheat Guard, build, run tests, check SHA-256 equivalence, run interleaved A/B pairs (`wall`, `rss`, or `pareto` with regression guardrails) |
| `oa_checkpoint` | `oa checkpoint` | Advance `refs/oa/best` on verified wins or revert working tree and log dead ends |
| `oa_verify` | `oa verify` | Fresh end-to-end verification of `HEAD` vs `refs/oa/baseline` with CI exit code and badge |
| `oa_status` | `oa status` | Live session metric, cumulative gain, Cheat Guard lock health, dirty files, diff stat |
| `oa_ledger` | `oa ledger` | Inspect experiment history, Pareto frontier IDs, and ruled-out dead ends |
| `oa_report` | `oa report` | Export a PR-ready Markdown report, raw JSON ledger, or dark/light SVG chart (`--format svg`) |
| `oa_diff` | `oa diff` | Export the clean unified patch and diff stat between `refs/oa/baseline` and `refs/oa/best` |
| `oa_compare` | `oa compare` | Standalone interleaved A/B comparison of two commands or `--git <rev>[..<rev2>]` (`--metric wall\|rss\|pareto`, `--max-rss-regression`, `--markdown`, `--svg`, `--csv`, `--env`, `--check-output`) |

Autonomous loop and utility commands (CLI): `oa run` (autonomous `discover -> profile -> agent -> evaluate -> keep/revert` loop with `--agent claude|codex|gemini|opencode|cmd` and `--metric pareto`), `oa demo` (self-contained walkthrough), `oa doctor` (local profiler & agent check), `oa setup` (auto-configure MCP + `SKILL.md`), `oa clean [--refs]` (remove `.oa/` shadow worktrees and session state while keeping working tree edits).

## FAQ

<details>
<summary><strong>Does oa touch my git history or push anything?</strong></summary>

No. `oa` stores snapshots under private git refs (`refs/oa/baseline`, `refs/oa/best`, `refs/oa/exp/<n>`) and shadow worktrees under `.oa/` (automatically added to `.git/info/exclude`). Your branch history and remotes stay untouched. Run `oa clean --refs` after committing to remove `.oa/` and all `refs/oa/*` refs.

</details>

<details>
<summary><strong>How does Windows Peak RSS measurement work without unsafe Rust?</strong></summary>

On Linux and macOS, `oa` uses `/usr/bin/time` (or `python3` `resource.getrusage`). On Windows, `oa` wraps `--metric rss` executions via stdlib Python `ctypes` calling Win32 `K32GetProcessMemoryInfo` and `QueryInformationJobObject` (`PeakJobMemoryUsed`), keeping the Rust binary `#![forbid(unsafe_code)]`.

</details>

<details>
<summary><strong>What if my workload has nondeterministic output (timestamps, UUIDs, floats)?</strong></summary>

Pass `--ignore-line <SUBSTR>` to strip dynamic lines before hashing, or pass `--oracle "<CMD>"` to delegate equivalence checking to a custom validator script (for example, numeric tolerance `rtol`/`atol`).

</details>

<details>
<summary><strong>Does oa require root for Linux perf?</strong></summary>

No. When `/proc/sys/kernel/perf_event_paranoid` blocks kernel events, `oa` retries automatically with user-space `:u` counters (`cycles:u`, `instructions:u`, `branches:u`, `branch-misses:u`) and user-space `perf record`. Compile native targets with `-g` or `CARGO_PROFILE_RELEASE_DEBUG=true` to resolve ELF hotspots to project `file:line` snippets.

</details>

## Limitations

- Single-machine local benchmarking only; distributed and multi-node latency profiles are out of scope.
- `--metric rss` on Windows requires `python` on `PATH` (for the stdlib `ctypes` Win32 memory query); Linux and macOS use `/usr/bin/time` or `python3`.
- Very fast commands (`< 15 ms`) are dominated by process-spawn overhead (`oa` warns automatically when candidate median drops below `15 ms` or CV exceeds `8%`); put an inner loop in the benchmark script so measured work is at least `30-50 ms`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development commands (`cargo test --all-targets`, `cargo clippy --all-targets -- -D warnings`), the profiler adapter contract, and open `good first issue` tickets.

## License

[MIT](LICENSE)
