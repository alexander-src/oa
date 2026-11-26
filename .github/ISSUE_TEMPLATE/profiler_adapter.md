---
name: New Profiler Adapter
about: Propose or claim a new language or OS profiler adapter for src/profiler.rs
labels: enhancement, good first issue
---

## Target Runtime or OS

Example: Go (`go test -cpuprofile`), Ruby (`stackprof`), macOS (`sample` / `xctrace`), Windows (ETW).

## Command Pattern to Detect

Example: `go test ...` or `go run ...`

## Profiler Output Format

How hotspots, line numbers, and optional allocation sites will be extracted without adding external crate dependencies.
