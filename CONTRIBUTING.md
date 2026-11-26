# Contributing to oa

`oa` is a small, zero-unsafe Rust codebase (`#![forbid(unsafe_code)]`, only `serde` and `serde_json` as direct dependencies). Pull requests are welcome, especially new profiler adapters, agent setup targets, and statistical or reporting improvements.

## Local Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

## Architecture

- `src/sha256.rs`: pure-Rust SHA-256 used for Cheat Guard locks and output equivalence.
- `src/stats.rs`: Mann-Whitney U test (tie and continuity corrected) and deterministic 2,000-resample bootstrap 95% confidence intervals.
- `src/runner.rs`: subprocess execution, Linux `taskset` CPU core pinning, and peak RSS capture (`/usr/bin/time`, macOS `/usr/bin/time -l`, and Unix `python3` `resource.getrusage` fallback).
- `src/git.rs`: non-invasive `refs/oa/*` tree snapshots and detached shadow worktrees under `.oa/worktrees/`.
- `src/guard.rs`: Cheat Guard file locks and `Exact` / `Normalized` / `Oracle` equivalence verification.
- `src/profiler.rs`: runtime profiler adapters returning a unified `ProfileReport` and hotspot diffs.
- `src/session.rs`: session orchestration (`init`, `profile`, `evaluate`, `checkpoint`, `ledger`, `report`).
- `src/mcp.rs`: stdio JSON-RPC 2.0 MCP server (NDJSON and `Content-Length` framing).
- `src/setup.rs`: cross-agent MCP and `SKILL.md` installer.

## Adding a Profiler Adapter

Every language or OS profiler in `src/profiler.rs` follows the same contract:

1. Detect the command invocation (for example `detect_go_invocation` or `detect_ruby_invocation`).
2. Run the runtime's built-in profiler writing output into a unique temp file or directory using `PROF_SEQ`.
3. Parse the profile into `ProfileReport` (`hotspots`, `alloc_sites`, `bottleneck_class`, `diagnosis`) and attach source snippets for the top project frames via `read_source_snippet`.
4. Register the new `--mode <name>` option in `run_profile`, `src/mcp.rs`, and `src/main.rs`.
5. Add a unit test in `tests/stats_and_sha.rs` parsing a representative sample profile payload so CI validates it on Linux, macOS, and Windows without requiring that runtime to be installed.

## Adding an Agent Target in `oa setup`

Add a new `AgentTarget` entry in `discover_targets()` inside `src/setup.rs` with its config path and optional `SKILL.md` destination. All config writes must preserve unrelated keys, back up the original file to `.oa.bak`, and honor `--dry-run`.

## PR Checklist

- Keep `#![forbid(unsafe_code)]` intact and avoid adding unnecessary crates.
- Make sure `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all-targets` pass.
- Include tests for any new parser, guard rule, or statistical behavior.
