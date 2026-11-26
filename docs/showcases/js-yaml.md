# Showcase: nodeca/js-yaml (1.77x faster `yaml.load`, -32.8% peak RSS)

- Target: [nodeca/js-yaml](https://github.com/nodeca/js-yaml) (`v5.5.0`, commit `3edb91fbc7`, 80M+ weekly npm downloads)
- Profiler backend: `node:v8-cpu-prof` (Node.js v22.14.0)
- Host: `linux x86_64`, AMD Ryzen 7 9800X3D
- Verification: 1,633 upstream core + official YAML 1.2 spec tests pass (13 skipped, 0 failed), `tsc --noEmit` clean, `eslint .` clean, 34 locked files intact, byte-identical output SHA-256 (`aa267bde21515373`)
- Apply-ready patch: [`js-yaml.patch`](js-yaml.patch)

## Verified results

Workload: official `benchmark/samples/document_huge.yaml` (6.9 MB) loaded via `yaml.load`, plus 20x `yaml.load` + `yaml.dump` round-trips across all 13 remaining files in `benchmark/samples/`, hashed deterministically with SHA-256.

| Metric | Baseline (`v5.5.0`) | After `oa` | Delta |
| --- | ---: | ---: | ---: |
| Pure `yaml.load(document_huge.yaml)` (median of 10 runs) | 78.26 ms | 44.22 ms | **1.77x** (~156 MB/s) |
| Full CLI (`oa verify --pairs 20`, incl. Node startup + `JSON.stringify` + SHA-256) | 184.97 ms | 127.43 ms | **1.37x** (`95% CI [1.35x, 1.43x]`, `p = 0.0000`) |
| Peak RSS | 179,896 KiB (175.7 MiB) | 120,012 KiB (117.2 MiB) | **-32.8%** (-58.5 MiB) |
| Upstream test suite (core + YAML 1.2 spec) | 1,633 pass, 0 fail | 1,633 pass, 0 fail | 0 regressions |

## What `node:v8-cpu-prof` found

Initial V8 CPU profiling flagged `GC_OR_ALLOC_BOUND` and `SERIALIZATION_BOUND`:

1. `parseNode` allocated `emptyProperties()` and a 6-field `snapshotState()` object on every single node even when no `!` tag or `&` anchor was present, and `readBlockMapping` snapshotted every key even after the mapping was already opened.
2. `parseEvents` buffered hundreds of thousands of heap-allocated JS event objects (`ScalarEvent`, `MappingEvent`, `SequenceEvent`, `PopEvent`) in `Event[]` before `constructFromEvents` ran, driving V8 garbage collection to 16.4% of total CPU time and bloating peak RSS to 180 MiB for a 6.9 MB file.
3. `checkPrintable` sliced every scalar and executed a backtracking surrogate RegExp (`PATTERN_NON_PRINTABLE`) on every plain scalar in the document.
4. `resolveImplicitScalarTag` called `source.charAt(0)` and `Map.get()` on every plain scalar, while `int_core.ts` and `float_core.ts` ran full RegExps on simple integers and non-numeric strings.

## Experiment ledger

| # | Status | Hypothesis | Files | Paired gain | 95% CI | p-value | Median ms |
| ---: | --- | --- | --- | ---: | --- | ---: | ---: |
| 1 | KEPT | Lazy `parseNode` properties/snapshots, shared `EMPTY_PROPS`/`POP_EVENT`, register-cached `readPlainScalar`, `readDoubleQuotedScalar`, and `skipSeparationSpace` | `src/parser/parser.ts` | 1.17x | [1.16x, 1.22x] | 0.0000 | 148.20 |
| 2 | REVERTED | Whole-input `PATTERN_NON_PRINTABLE.test(input)` upfront check + `Event[]` constructor tweaks | `src/parser/constructor.ts`, `src/parser/parser.ts`, `src/schema.ts`, `src/tag/scalar/int_core.ts` | 1.02x | [1.01x, 1.03x] | 0.0404 | 149.12 |
| 3 | KEPT | Packed `Int32Array` event tape (`src/parser/tape.ts`) for `load`/`loadAll` (zero JS event objects), inline ASCII printability validation in `readPlainScalar`, 128-entry `charCodeAt` schema dispatch, and zero-RegExp integer/float fast paths | 7 files under `src/` | 1.17x | [1.16x, 1.18x] | 0.0000 | 127.43 |

### Dead end caught by `oa`

Experiment #2 replaced per-scalar printability regexes with a single upfront `PATTERN_NON_PRINTABLE.test(input)` scan over the 6.9 MB file. `oa evaluate` rejected it (`NoiseReject`, `1.02x`, `p = 0.0404`) because the surrogate lookahead regex still took 5.3 ms over 6.9 MB while `Event[]` object churn kept GC at 16.5%. Experiment #3 instead folded ASCII printability into `readPlainScalar`'s existing `Uint8Array(128)` stop table (0 ms extra cost) and replaced `Event[]` inside `load()` with a flat non-GC-traced `Int32Array` tape, cutting GC time by 51.6% and Peak RSS by 32.8% while keeping the public `parseEvents()` API unchanged.

### Profile diff vs baseline (`oa profile --diff-from baseline`)

```text
Diff vs prior: 199.39 ms -> 138.90 ms (-30.3%)
    -31.90 ms ( -78.8%)  parseNode (dist/js-yaml.mjs)
    -15.71 ms ( -51.6%)  (garbage collector) ()
     -9.03 ms (-100.0%)  RegExp: [\x00-\x08...] ()
     -6.50 ms (-100.0%)  constructFromEvents (dist/js-yaml.mjs)
```

## Reproduce

```bash
git clone https://github.com/nodeca/js-yaml.git
cd js-yaml
git checkout 3edb91fbc78a10a8ddc15d2e82c8f6d0baa1c3d3
npm ci
git apply /path/to/oa/docs/showcases/js-yaml.patch
npm run build
npm run lint
npm run type-check
npm run spec:get
node --experimental-strip-types --test 'test/core/**/*.test.mjs' 'test/spec/*.test.mjs'
```
